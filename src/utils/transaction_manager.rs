use std::future::Future;
use std::time::Duration;
use tokio::sync::{mpsc, watch};
use tokio::time::{sleep_until, Instant};

pub type TransactionSignatureBytes = [u8; 64];

#[derive(Clone)]
pub struct ActiveTransaction {
    pub signature: TransactionSignatureBytes,
    pub landed_tx: mpsc::Sender<LandedTransaction>,
}

#[derive(Debug, Clone, Copy)]
pub struct LandedTransaction {
    pub slot_landed: u64,
    pub observed_at: Instant,
}

#[derive(Debug, Clone, Copy)]
pub struct ConfirmedTransaction {
    pub slot_landed: u64,
    pub latency: Duration,
}

#[derive(Debug, Clone, Copy)]
pub enum SendAndConfirmOutcome {
    Confirmed(ConfirmedTransaction),
    TimedOut,
}

pub async fn send_and_confirm<F, Fut, R>(
    signature: TransactionSignatureBytes,
    active_transaction_tx: &watch::Sender<Option<ActiveTransaction>>,
    confirmation_timeout: Duration,
    resend_interval: Duration,
    mut send_transaction: F,
    mut record_retry: R,
) -> SendAndConfirmOutcome
where
    F: FnMut() -> Fut,
    Fut: Future<Output = bool>,
    R: FnMut(u64),
{
    let (landed_tx, mut landed_rx) = mpsc::channel(1);
    active_transaction_tx.send_replace(Some(ActiveTransaction {
        signature,
        landed_tx,
    }));

    let send_started_at = Instant::now();
    let confirmation_deadline = send_started_at + confirmation_timeout;
    let mut retry_number = 0u64;

    let outcome = 'confirmation: loop {
        if Instant::now() >= confirmation_deadline {
            break SendAndConfirmOutcome::TimedOut;
        }

        let send_deadline = (Instant::now() + resend_interval).min(confirmation_deadline);
        let send_succeeded = tokio::select! {
            biased;
            Some(landed) = landed_rx.recv() => {
                break 'confirmation confirmed_transaction(landed, send_started_at);
            }
            _ = sleep_until(confirmation_deadline) => {
                break 'confirmation SendAndConfirmOutcome::TimedOut;
            }
            _ = sleep_until(send_deadline) => {
                false
            }
            result = send_transaction() => result,
        };

        if !send_succeeded {
            retry_number = retry_number.saturating_add(1);
            record_retry(retry_number);
            tokio::task::yield_now().await;
            continue;
        }

        let retry_at = (Instant::now() + resend_interval).min(confirmation_deadline);
        tokio::select! {
            biased;
            Some(landed) = landed_rx.recv() => {
                break 'confirmation confirmed_transaction(landed, send_started_at);
            }
            _ = sleep_until(confirmation_deadline) => {
                break 'confirmation SendAndConfirmOutcome::TimedOut;
            }
            _ = sleep_until(retry_at) => {
                retry_number = retry_number.saturating_add(1);
                record_retry(retry_number);
            }
        }
    };

    active_transaction_tx.send_replace(None);
    outcome
}

fn confirmed_transaction(
    landed: LandedTransaction,
    send_started_at: Instant,
) -> SendAndConfirmOutcome {
    let latency = landed
        .observed_at
        .checked_duration_since(send_started_at)
        .unwrap_or_default();
    SendAndConfirmOutcome::Confirmed(ConfirmedTransaction {
        slot_landed: landed.slot_landed,
        latency,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    #[tokio::test(start_paused = true)]
    async fn retries_immediately_after_send_error() {
        let (active_tx, _active_rx) = watch::channel(None);
        let attempts = Arc::new(AtomicUsize::new(0));
        let attempts_for_send = Arc::clone(&attempts);
        let retries = Arc::new(AtomicUsize::new(0));
        let retries_for_record = Arc::clone(&retries);

        let task = tokio::spawn(async move {
            send_and_confirm(
                [1; 64],
                &active_tx,
                Duration::from_secs(1),
                Duration::from_millis(100),
                move || {
                    let attempt = attempts_for_send.fetch_add(1, Ordering::SeqCst);
                    async move { attempt > 0 }
                },
                move |_| {
                    retries_for_record.fetch_add(1, Ordering::SeqCst);
                },
            )
            .await
        });

        tokio::task::yield_now().await;
        tokio::task::yield_now().await;
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
        assert_eq!(retries.load(Ordering::SeqCst), 1);

        tokio::time::advance(Duration::from_secs(1)).await;
        assert!(matches!(
            task.await.unwrap(),
            SendAndConfirmOutcome::TimedOut
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn successful_send_waits_before_retrying() {
        let (active_tx, active_rx) = watch::channel(None);
        let attempts = Arc::new(AtomicUsize::new(0));
        let attempts_for_send = Arc::clone(&attempts);
        let retries = Arc::new(AtomicUsize::new(0));
        let retries_for_record = Arc::clone(&retries);

        let task = tokio::spawn(async move {
            send_and_confirm(
                [3; 64],
                &active_tx,
                Duration::from_secs(5),
                Duration::from_secs(2),
                move || {
                    attempts_for_send.fetch_add(1, Ordering::SeqCst);
                    async { true }
                },
                move |_| {
                    retries_for_record.fetch_add(1, Ordering::SeqCst);
                },
            )
            .await
        });

        tokio::task::yield_now().await;
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
        assert_eq!(retries.load(Ordering::SeqCst), 0);

        tokio::time::advance(Duration::from_millis(1_999)).await;
        tokio::task::yield_now().await;
        assert_eq!(attempts.load(Ordering::SeqCst), 1);

        tokio::time::advance(Duration::from_millis(1)).await;
        tokio::task::yield_now().await;
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
        assert_eq!(retries.load(Ordering::SeqCst), 1);

        let active = active_rx.borrow().clone().unwrap();
        active
            .landed_tx
            .try_send(LandedTransaction {
                slot_landed: 44,
                observed_at: Instant::now(),
            })
            .unwrap();
        assert!(matches!(
            task.await.unwrap(),
            SendAndConfirmOutcome::Confirmed(_)
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn matching_landing_uses_observation_time() {
        let (active_tx, active_rx) = watch::channel(None);
        let started_at = Instant::now();

        let task = tokio::spawn(async move {
            send_and_confirm(
                [2; 64],
                &active_tx,
                Duration::from_secs(5),
                Duration::from_secs(2),
                || async { true },
                |_| {},
            )
            .await
        });

        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_millis(450)).await;

        let active = active_rx.borrow().clone().unwrap();
        active
            .landed_tx
            .try_send(LandedTransaction {
                slot_landed: 42,
                observed_at: Instant::now(),
            })
            .unwrap();

        let SendAndConfirmOutcome::Confirmed(confirmed) = task.await.unwrap() else {
            panic!("transaction should be confirmed");
        };
        assert_eq!(confirmed.slot_landed, 42);
        assert_eq!(confirmed.latency, Instant::now() - started_at);
    }
}
