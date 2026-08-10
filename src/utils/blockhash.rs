use anyhow::{Context, Result};
use futures::StreamExt;
use log::error;
use solana_sdk::hash::Hash;
use std::collections::HashMap;
use tokio::sync::{oneshot, watch};
use tokio::time::Instant;
use yellowstone_grpc_client::GeyserGrpcClient;
use yellowstone_grpc_proto::prelude::{
    subscribe_update::UpdateOneof, CommitmentLevel, SubscribeRequest,
    SubscribeRequestFilterBlocksMeta,
};

#[derive(Debug, Clone, Copy)]
pub struct BlockhashSnapshot {
    pub value: Hash,
    pub observed_at: Instant,
}

/// Watches blockhash updates via gRPC block_meta subscription
pub async fn watch_blockhash(
    mut grpc_client: GeyserGrpcClient,
    blockhash_tx: watch::Sender<Option<BlockhashSnapshot>>,
    ready_tx: oneshot::Sender<()>,
    commitment: CommitmentLevel,
) -> Result<()> {
    // Create subscription request for block_meta
    let mut blocks_filter = HashMap::new();
    blocks_filter.insert(
        "block_meta".to_string(),
        SubscribeRequestFilterBlocksMeta {},
    );

    let subscribe_request = SubscribeRequest {
        blocks_meta: blocks_filter,
        commitment: Some(commitment.into()),
        ..Default::default()
    };

    let mut stream = grpc_client
        .subscribe_once(subscribe_request)
        .await
        .context("Failed to create block_meta subscription")?;
    let _ = ready_tx.send(());

    let mut previous_hash = None;

    // Process stream updates
    while let Some(message) = stream.next().await {
        match message {
            Ok(msg) => {
                if let Some(UpdateOneof::BlockMeta(block_meta_update)) = msg.update_oneof {
                    let blockhash_str = block_meta_update.blockhash;
                    // Parse blockhash from base58 string
                    let hash_bytes = match bs58::decode(&blockhash_str).into_vec() {
                        Ok(decoded) => {
                            if decoded.len() == 32 {
                                match <[u8; 32]>::try_from(decoded.as_slice()) {
                                    Ok(arr) => arr,
                                    Err(_) => {
                                        error!(
                                            "[Blockhash Watcher] Failed to convert decoded blockhash to array for blockhash {:?} with length {:?}",
                                            blockhash_str, decoded.len()
                                        );
                                        continue;
                                    }
                                }
                            } else {
                                error!(
                                    "[Blockhash Watcher] Decoded blockhash has wrong length: {:?} (expected 32)",
                                    decoded.len()
                                );
                                continue;
                            }
                        }
                        Err(e) => {
                            error!("[Blockhash Watcher] Failed to decode blockhash: {:?}", e);
                            continue;
                        }
                    };

                    let new_hash = Hash::new_from_array(hash_bytes);

                    if previous_hash != Some(new_hash) {
                        previous_hash = Some(new_hash);
                        blockhash_tx.send_replace(Some(BlockhashSnapshot {
                            value: new_hash,
                            observed_at: Instant::now(),
                        }));
                    }
                }
            }
            Err(e) => {
                return Err(anyhow::anyhow!(
                    "[Blockhash Watcher] Fatal stream error: {:?}",
                    e
                ));
            }
        }
    }

    Err(anyhow::anyhow!("[Blockhash Watcher] Stream ended"))
}
