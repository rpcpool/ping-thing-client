use anyhow::Result;
use log::{debug, error, info, warn};
use reqwest::Client;
use serde::Deserialize;
use serde_json::json;
use tokio::sync::watch;
use tokio::time::{sleep, Duration, Instant};

#[derive(Debug, Clone, Copy)]
pub struct PriorityFeeSnapshot {
    pub value: u64,
    pub observed_at: Instant,
}

#[derive(Debug, Deserialize)]
struct PrioritizationFee {
    #[allow(dead_code)]
    slot: u64,
    #[serde(rename = "prioritizationFee")]
    prioritization_fee: u64,
}

#[derive(Debug, Deserialize)]
struct RpcResponse {
    #[allow(dead_code)]
    jsonrpc: String,
    result: Vec<PrioritizationFee>,
    #[allow(dead_code)]
    id: serde_json::Value,
}

/// Watches prioritization fees by polling RPC every 350ms
pub async fn watch_prioritization_fees(
    rpc_endpoint: String,
    priority_fee_tx: watch::Sender<Option<PriorityFeeSnapshot>>,
    percentile: u16,
) -> Result<()> {
    info!(
        "[Priority Fees Watcher] Starting with percentile: {:?}",
        percentile
    );

    let client = Client::new();

    loop {
        // Make JSON-RPC call to getRecentPrioritizationFees
        let payload = json!({
            "jsonrpc": "2.0",
            "id": "1",
            "method": "getRecentPrioritizationFees",
            "params": [
                [],
                {
                    "percentile": percentile
                }
            ]
        });

        match client
            .post(&rpc_endpoint)
            .header("Content-Type", "application/json")
            .json(&payload)
            .send()
            .await
        {
            Ok(response) => {
                if response.status().is_success() {
                    match response.json::<RpcResponse>().await {
                        Ok(rpc_response) => {
                            // Process the fee results
                            if !rpc_response.result.is_empty() {
                                let Some(max_fee) = rpc_response
                                    .result
                                    .iter()
                                    .map(|f| f.prioritization_fee)
                                    .max()
                                else {
                                    continue;
                                };

                                let previous_fee =
                                    priority_fee_tx.borrow().as_ref().map(|fee| fee.value);
                                priority_fee_tx.send_replace(Some(PriorityFeeSnapshot {
                                    value: max_fee,
                                    observed_at: Instant::now(),
                                }));

                                if previous_fee != Some(max_fee) {
                                    debug!(
                                        "[Priority Fees Watcher] Updated priority fee: {:?} (previous: {:?})",
                                        max_fee, previous_fee
                                    );
                                }
                            } else {
                                warn!("[Priority Fees Watcher] Received empty fee results");
                            }
                        }
                        Err(e) => {
                            error!(
                                "[Priority Fees Watcher] Failed to parse RPC response from {:?}: {:?}",
                                rpc_endpoint, e
                            );
                        }
                    }
                } else {
                    error!(
                        "[Priority Fees Watcher] RPC request to {:?} failed with status: {:?}",
                        rpc_endpoint,
                        response.status()
                    );
                }
            }
            Err(e) => {
                error!(
                    "[Priority Fees Watcher] HTTP request to {:?} failed: {:?}",
                    rpc_endpoint, e
                );
                // println!("{:?}", e)
            }
        }

        // Sleep for 350ms before next poll
        sleep(Duration::from_millis(350)).await;
    }
}
