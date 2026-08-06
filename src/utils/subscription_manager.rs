use anyhow::{Context, Result};
use futures::StreamExt;
use log::info;
use solana_pubkey::Pubkey;
use std::collections::HashMap;
use tokio::sync::{oneshot, watch};
use tokio::time::Instant;
use yellowstone_grpc_client::GeyserGrpcClient;
use yellowstone_grpc_proto::prelude::{
    subscribe_update::UpdateOneof, CommitmentLevel, SubscribeRequest,
    SubscribeRequestFilterTransactions,
};

use super::transaction_manager::{ActiveTransaction, LandedTransaction};

/// Watches all transactions for a specific wallet pubkey via gRPC subscription
pub async fn watch_transactions(
    mut grpc_client: GeyserGrpcClient,
    active_transaction_rx: watch::Receiver<Option<ActiveTransaction>>,
    ready_tx: oneshot::Sender<()>,
    wallet_pubkey: Pubkey,
    commitment: CommitmentLevel,
) -> Result<()> {
    info!(
        "[Transaction Watcher] Starting transaction watching for wallet: {:?}",
        wallet_pubkey
    );

    // Create subscription request for all transactions involving the wallet
    let mut transactions_filter = HashMap::new();
    transactions_filter.insert(
        "wallet_transactions".to_string(),
        SubscribeRequestFilterTransactions {
            vote: Some(false),
            failed: Some(false),
            signature: None, // Watch all transactions, not a specific one
            account_include: vec![wallet_pubkey.to_string()],
            account_exclude: vec![],
            account_required: vec![wallet_pubkey.to_string()],
            token_accounts: None,
        },
    );

    let subscribe_request = SubscribeRequest {
        transactions: transactions_filter,
        commitment: Some(commitment.into()),
        ..Default::default()
    };

    let mut stream = grpc_client
        .subscribe_once(subscribe_request)
        .await
        .context("Failed to create transaction subscription")?;

    info!("[Transaction Watcher] Successfully subscribed to transaction stream");
    let _ = ready_tx.send(());

    while let Some(message) = stream.next().await {
        match message {
            Ok(msg) => {
                if let Some(UpdateOneof::Transaction(tx_update)) = msg.update_oneof {
                    if let Some(transaction) = tx_update.transaction {
                        notify_if_active_signature_matches(
                            &transaction.signature,
                            tx_update.slot,
                            &active_transaction_rx,
                        );
                    }
                }
            }
            Err(e) => {
                return Err(anyhow::anyhow!(
                    "[Transaction Watcher] Fatal stream error for wallet {:?}: {:?}",
                    wallet_pubkey,
                    e
                ));
            }
        }
    }
    Err(anyhow::anyhow!(
        "[Transaction Watcher] Stream ended for wallet {:?}",
        wallet_pubkey
    ))
}

fn notify_if_active_signature_matches(
    transaction_signature: &[u8],
    slot_landed: u64,
    active_transaction_rx: &watch::Receiver<Option<ActiveTransaction>>,
) {
    let active_transaction = active_transaction_rx.borrow();
    let Some(active_transaction) = active_transaction.as_ref() else {
        return;
    };

    if transaction_signature != active_transaction.signature {
        return;
    }

    let observed_at = Instant::now();
    let _ = active_transaction.landed_tx.try_send(LandedTransaction {
        slot_landed,
        observed_at,
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc;

    #[tokio::test]
    async fn only_matching_signature_is_reported_as_landed() {
        let (landed_tx, mut landed_rx) = mpsc::channel(1);
        let (_active_tx, active_rx) = watch::channel(Some(ActiveTransaction {
            signature: [7; 64],
            landed_tx,
        }));

        notify_if_active_signature_matches(&[8; 64], 10, &active_rx);
        assert!(landed_rx.try_recv().is_err());

        notify_if_active_signature_matches(&[7; 64], 11, &active_rx);
        let landed = landed_rx.recv().await.unwrap();
        assert_eq!(landed.slot_landed, 11);
    }
}
