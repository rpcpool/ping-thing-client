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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryReason {
    SendFailed,
    ConfirmationWaitExpired,
}

enum SendAttemptOutcome {
    Succeeded,
    Failed,
}

pub async fn send_and_confirm<F, Fut, A, AttemptFut, R>(
    signature: TransactionSignatureBytes,
    active_transaction_tx: &watch::Sender<Option<ActiveTransaction>>,
    confirmation_timeout: Duration,
    resend_interval: Duration,
    mut before_send_attempt: A,
    mut send_transaction: F,
    mut record_retry: R,
) -> SendAndConfirmOutcome
where
    F: FnMut(u64) -> Fut,
    Fut: Future<Output = bool>,
    A: FnMut(u64) -> AttemptFut,
    AttemptFut: Future<Output = ()>,
    R: FnMut(u64, RetryReason),
{
    let (landed_tx, mut landed_rx) = mpsc::channel(1);
    active_transaction_tx.send_replace(Some(ActiveTransaction {
        signature,
        landed_tx,
    }));

    let mut send_started_at = None;
    let mut confirmation_deadline = None;
    let mut retry_number = 0u64;
    let mut attempt_number = 0u64;

    let outcome = 'confirmation: loop {
        if let Some(confirmation_deadline) = confirmation_deadline {
            if Instant::now() >= confirmation_deadline {
                break SendAndConfirmOutcome::TimedOut;
            }
        }
        if let Ok(landed) = landed_rx.try_recv() {
            let send_started_at = send_started_at.unwrap_or(landed.observed_at);
            break confirmed_transaction(landed, send_started_at);
        }

        attempt_number = attempt_number.saturating_add(1);
        before_send_attempt(attempt_number).await;

        let first_send_started_at = *send_started_at.get_or_insert_with(Instant::now);
        let confirmation_deadline =
            *confirmation_deadline.get_or_insert(first_send_started_at + confirmation_timeout);
        let send_outcome = tokio::select! {
            biased;
            Some(landed) = landed_rx.recv() => {
                break 'confirmation confirmed_transaction(landed, first_send_started_at);
            }
            _ = sleep_until(confirmation_deadline) => {
                break 'confirmation SendAndConfirmOutcome::TimedOut;
            }
            result = send_transaction(attempt_number) => {
                if result {
                    SendAttemptOutcome::Succeeded
                } else {
                    SendAttemptOutcome::Failed
                }
            },
        };

        if matches!(send_outcome, SendAttemptOutcome::Failed) {
            retry_number = retry_number.saturating_add(1);
            record_retry(retry_number, RetryReason::SendFailed);
            tokio::task::yield_now().await;
            continue;
        }

        let retry_at = Instant::now()
            .checked_add(resend_interval)
            .unwrap_or(confirmation_deadline)
            .min(confirmation_deadline);
        tokio::select! {
            biased;
            Some(landed) = landed_rx.recv() => {
                break 'confirmation confirmed_transaction(landed, first_send_started_at);
            }
            _ = sleep_until(confirmation_deadline) => {
                break 'confirmation SendAndConfirmOutcome::TimedOut;
            }
            _ = sleep_until(retry_at) => {
                retry_number = retry_number.saturating_add(1);
                record_retry(retry_number, RetryReason::ConfirmationWaitExpired);
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

    fn retry_reason_code(reason: RetryReason) -> usize {
        match reason {
            RetryReason::SendFailed => 1,
            RetryReason::ConfirmationWaitExpired => 2,
        }
    }

    #[tokio::test(start_paused = true)]
    async fn retries_immediately_after_send_error() {
        let (active_tx, _active_rx) = watch::channel(None);
        let attempts = Arc::new(AtomicUsize::new(0));
        let attempts_for_send = Arc::clone(&attempts);
        let retries = Arc::new(AtomicUsize::new(0));
        let retries_for_record = Arc::clone(&retries);
        let last_retry_reason = Arc::new(AtomicUsize::new(0));
        let last_retry_reason_for_record = Arc::clone(&last_retry_reason);

        let task = tokio::spawn(async move {
            send_and_confirm(
                [1; 64],
                &active_tx,
                Duration::from_secs(1),
                Duration::from_millis(100),
                |_| async {},
                move |_| {
                    let attempt = attempts_for_send.fetch_add(1, Ordering::SeqCst);
                    async move { attempt > 0 }
                },
                move |_, reason| {
                    retries_for_record.fetch_add(1, Ordering::SeqCst);
                    last_retry_reason_for_record.store(retry_reason_code(reason), Ordering::SeqCst);
                },
            )
            .await
        });

        tokio::task::yield_now().await;
        tokio::task::yield_now().await;
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
        assert_eq!(retries.load(Ordering::SeqCst), 1);
        assert_eq!(last_retry_reason.load(Ordering::SeqCst), 1);

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
        let last_retry_reason = Arc::new(AtomicUsize::new(0));
        let last_retry_reason_for_record = Arc::clone(&last_retry_reason);

        let task = tokio::spawn(async move {
            send_and_confirm(
                [3; 64],
                &active_tx,
                Duration::from_secs(5),
                Duration::from_secs(2),
                |_| async {},
                move |_| {
                    attempts_for_send.fetch_add(1, Ordering::SeqCst);
                    async { true }
                },
                move |_, reason| {
                    retries_for_record.fetch_add(1, Ordering::SeqCst);
                    last_retry_reason_for_record.store(retry_reason_code(reason), Ordering::SeqCst);
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
        assert_eq!(last_retry_reason.load(Ordering::SeqCst), 2);

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
                |_| async {},
                |_| async { true },
                |_, _| {},
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

    #[tokio::test(start_paused = true)]
    async fn slow_send_is_not_cancelled_at_resend_interval() {
        let (active_tx, active_rx) = watch::channel(None);
        let attempts = Arc::new(AtomicUsize::new(0));
        let attempts_for_send = Arc::clone(&attempts);
        let completions = Arc::new(AtomicUsize::new(0));
        let completions_for_send = Arc::clone(&completions);

        let task = tokio::spawn(async move {
            send_and_confirm(
                [4; 64],
                &active_tx,
                Duration::from_secs(10),
                Duration::from_secs(2),
                |_| async {},
                move |_| {
                    attempts_for_send.fetch_add(1, Ordering::SeqCst);
                    let completions_for_send = Arc::clone(&completions_for_send);
                    async move {
                        tokio::time::sleep(Duration::from_secs(3)).await;
                        completions_for_send.fetch_add(1, Ordering::SeqCst);
                        true
                    }
                },
                |_, _| {},
            )
            .await
        });

        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(2)).await;
        tokio::task::yield_now().await;
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
        assert_eq!(completions.load(Ordering::SeqCst), 0);

        tokio::time::advance(Duration::from_secs(1)).await;
        tokio::task::yield_now().await;
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
        assert_eq!(completions.load(Ordering::SeqCst), 1);

        let active = active_rx.borrow().clone().unwrap();
        active
            .landed_tx
            .try_send(LandedTransaction {
                slot_landed: 45,
                observed_at: Instant::now(),
            })
            .unwrap();

        assert!(matches!(
            task.await.unwrap(),
            SendAndConfirmOutcome::Confirmed(_)
        ));
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn work_before_first_send_is_excluded_from_latency() {
        let (active_tx, active_rx) = watch::channel(None);

        let task = tokio::spawn(async move {
            send_and_confirm(
                [5; 64],
                &active_tx,
                Duration::from_secs(5),
                Duration::from_secs(2),
                |_| async {
                    tokio::time::sleep(Duration::from_millis(400)).await;
                },
                |_| async { true },
                |_, _| {},
            )
            .await
        });

        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_millis(400)).await;
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_millis(75)).await;

        let active = active_rx.borrow().clone().unwrap();
        active
            .landed_tx
            .try_send(LandedTransaction {
                slot_landed: 46,
                observed_at: Instant::now(),
            })
            .unwrap();

        let SendAndConfirmOutcome::Confirmed(confirmed) = task.await.unwrap() else {
            panic!("transaction should be confirmed");
        };
        assert_eq!(confirmed.latency, Duration::from_millis(75));
    }

    #[tokio::test(start_paused = true)]
    async fn oversized_resend_interval_uses_confirmation_deadline() {
        let (active_tx, _active_rx) = watch::channel(None);

        let task = tokio::spawn(async move {
            send_and_confirm(
                [6; 64],
                &active_tx,
                Duration::from_secs(1),
                Duration::MAX,
                |_| async {},
                |_| async { true },
                |_, _| {},
            )
            .await
        });

        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(1)).await;

        assert!(matches!(
            task.await.unwrap(),
            SendAndConfirmOutcome::TimedOut
        ));
    }
}
