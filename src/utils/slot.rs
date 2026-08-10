use anyhow::{Context, Result};
use futures::StreamExt;
use std::collections::HashMap;
use tokio::sync::{oneshot, watch};
use tokio::time::Instant;
use yellowstone_grpc_client::GeyserGrpcClient;
use yellowstone_grpc_proto::geyser::SlotStatus;
use yellowstone_grpc_proto::prelude::{
    subscribe_update::UpdateOneof, CommitmentLevel, SubscribeRequest, SubscribeRequestFilterSlots,
};

#[derive(Debug, Clone, Copy)]
pub struct SlotSnapshot {
    pub value: u64,
    pub observed_at: Instant,
}

/// Watches slot updates via gRPC subscription
pub async fn watch_slot(
    mut grpc_client: GeyserGrpcClient,
    slot_tx: watch::Sender<Option<SlotSnapshot>>,
    ready_tx: oneshot::Sender<()>,
    _commitment: CommitmentLevel,
) -> Result<()> {
    // Create subscription request for slots
    let mut slots_filter = HashMap::new();
    slots_filter.insert(
        "slots".to_string(),
        SubscribeRequestFilterSlots {
            filter_by_commitment: Some(false),
            interslot_updates: Some(true),
        },
    );

    let subscribe_request = SubscribeRequest {
        slots: slots_filter,
        ..Default::default()
    };

    let mut stream = grpc_client
        .subscribe_once(subscribe_request)
        .await
        .context("Failed to create slot subscription")?;
    let _ = ready_tx.send(());

    let mut message_count = 0u64;

    while let Some(message) = stream.next().await {
        message_count += 1;

        match message {
            Ok(msg) => {
                match msg.update_oneof {
                    Some(UpdateOneof::Slot(slot_update)) => {
                        // Only update slot on FIRST_SHRED_RECEIVED status
                        if let Ok(status) = SlotStatus::try_from(slot_update.status) {
                            if status == SlotStatus::SlotFirstShredReceived {
                                let slot = slot_update.slot;

                                slot_tx.send_replace(Some(SlotSnapshot {
                                    value: slot,
                                    observed_at: Instant::now(),
                                }));
                            }
                        }
                    }
                    _ => {
                        // Ignore other update types
                    }
                }
            }
            Err(e) => {
                return Err(anyhow::anyhow!(
                    "[Slot Watcher] Fatal stream error after {:?} messages: {:?}",
                    message_count,
                    e
                ));
            }
        }
    }

    Err(anyhow::anyhow!(
        "[Slot Watcher] Stream ended after processing {:?} messages",
        message_count
    ))
}
