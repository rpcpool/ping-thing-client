use anyhow::Result;
use log::{debug, info};
use prometheus::{
    Encoder, HistogramOpts, HistogramVec, IntCounterVec, IntGaugeVec, Opts, Registry,
};
use std::sync::Arc;
use warp::Filter;

pub struct Metrics {
    pub registry: Arc<Registry>,
    pub confirmation_latency: HistogramVec,
    pub slot_latency: HistogramVec,
    pub watcher_up: IntGaugeVec,
    pub watcher_fatal_errors: IntCounterVec,
    pub confirmation_channel_closed: IntCounterVec,
    pub transaction_timeouts: IntCounterVec,
    pub transaction_retries: HistogramVec,
    pub validators_app_send_failures: IntCounterVec,
}

impl Metrics {
    pub fn new() -> Result<Self> {
        info!("[Metrics] Initializing Prometheus metrics registry...");
        let registry = Arc::new(Registry::new());

        info!("[Metrics] Creating histogram buckets for confirmation latency...");
        // Generate millisecond buckets
        let mut buckets = Vec::new();
        for i in (0..=1000).step_by(50) {
            buckets.push(i as f64);
        }
        for i in (1100..=2000).step_by(100) {
            buckets.push(i as f64);
        }
        for i in (2200..=10000).step_by(200) {
            buckets.push(i as f64);
        }
        debug!(
            "[Metrics] Created {:?} buckets for confirmation latency",
            buckets.len()
        );

        info!("[Metrics] Registering confirmation_latency histogram...");
        let confirmation_latency = HistogramVec::new(
            HistogramOpts::new(
                "ping_thing_client_confirmation_latency",
                "Solana transaction confirmation latency in milliseconds",
            )
            .buckets(buckets),
            &["pinger_name"],
        )?;

        info!("[Metrics] Creating histogram buckets for slot latency...");
        let slot_buckets: Vec<f64> = (1..=30).map(|x| x as f64).collect();
        debug!(
            "[Metrics] Created {:?} buckets for slot latency",
            slot_buckets.len()
        );

        info!("[Metrics] Registering slot_latency histogram...");
        let slot_latency = HistogramVec::new(
            HistogramOpts::new(
                "ping_thing_client_slot_latency",
                "Difference between landed slot and sent slot",
            )
            .buckets(slot_buckets),
            &["pinger_name"],
        )?;

        let watcher_up = IntGaugeVec::new(
            Opts::new(
                "ping_thing_client_watcher_up",
                "Whether a watcher task is running",
            ),
            &["pinger_name", "watcher"],
        )?;

        let watcher_fatal_errors = IntCounterVec::new(
            Opts::new(
                "ping_thing_client_watcher_fatal_errors_total",
                "Fatal watcher task errors",
            ),
            &["pinger_name", "watcher"],
        )?;

        let confirmation_channel_closed = IntCounterVec::new(
            Opts::new(
                "ping_thing_client_confirmation_channel_closed_total",
                "Times the transaction update channel closed",
            ),
            &["pinger_name"],
        )?;

        let transaction_timeouts = IntCounterVec::new(
            Opts::new(
                "ping_thing_client_transaction_timeouts_total",
                "Transactions that were not confirmed before the local timeout",
            ),
            &["pinger_name"],
        )?;

        let transaction_retry_buckets: Vec<f64> = (1..=30).map(|retry| retry as f64).collect();
        let transaction_retries = HistogramVec::new(
            HistogramOpts::new(
                "ping_thing_client_transaction_retries",
                "Retry number observed each time a transaction is resent",
            )
            .buckets(transaction_retry_buckets),
            &["pinger_name"],
        )?;

        let validators_app_send_failures = IntCounterVec::new(
            Opts::new(
                "ping_thing_client_validators_app_send_failures_total",
                "Validators.app API send failures",
            ),
            &["pinger_name"],
        )?;

        info!("[Metrics] Registering metrics with Prometheus registry...");
        registry.register(Box::new(confirmation_latency.clone()))?;
        registry.register(Box::new(slot_latency.clone()))?;
        registry.register(Box::new(watcher_up.clone()))?;
        registry.register(Box::new(watcher_fatal_errors.clone()))?;
        registry.register(Box::new(confirmation_channel_closed.clone()))?;
        registry.register(Box::new(transaction_timeouts.clone()))?;
        registry.register(Box::new(transaction_retries.clone()))?;
        registry.register(Box::new(validators_app_send_failures.clone()))?;
        info!("[Metrics] All metrics registered successfully");

        Ok(Self {
            registry,
            confirmation_latency,
            slot_latency,
            watcher_up,
            watcher_fatal_errors,
            confirmation_channel_closed,
            transaction_timeouts,
            transaction_retries,
            validators_app_send_failures,
        })
    }

    pub async fn start_server(&self, port: u16) {
        info!(
            "[Metrics] Starting Prometheus metrics server on port {:?}...",
            port
        );
        let metrics = Arc::clone(&self.registry);

        let metrics_route = warp::path!("metrics").map(move || {
            debug!("[Metrics] Handling /metrics request");
            let metrics = Arc::clone(&metrics);
            let mut buffer = Vec::new();
            let encoder = prometheus::TextEncoder::new();
            encoder.encode(&metrics.gather(), &mut buffer).unwrap();
            String::from_utf8(buffer).unwrap()
        });

        info!(
            "[Metrics] Prometheus metrics server listening on :{:?}/metrics",
            port
        );
        warp::serve(metrics_route).run(([0, 0, 0, 0], port)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gathered_metrics_are_exposed_from_shared_registry() {
        let metrics = Metrics::new().unwrap();
        let mut buffer = Vec::new();
        let encoder = prometheus::TextEncoder::new();

        metrics
            .confirmation_latency
            .with_label_values(&["test-pinger"])
            .observe(1.0);
        metrics
            .slot_latency
            .with_label_values(&["test-pinger"])
            .observe(1.0);
        metrics
            .watcher_up
            .with_label_values(&["test-pinger", "transaction"])
            .set(1);
        metrics
            .watcher_fatal_errors
            .with_label_values(&["test-pinger", "transaction"])
            .inc();
        metrics
            .confirmation_channel_closed
            .with_label_values(&["test-pinger"])
            .inc();
        metrics
            .transaction_timeouts
            .with_label_values(&["test-pinger"])
            .inc();
        metrics
            .transaction_retries
            .with_label_values(&["test-pinger"])
            .observe(1.0);
        metrics
            .transaction_retries
            .with_label_values(&["test-pinger"])
            .observe(2.0);
        metrics
            .validators_app_send_failures
            .with_label_values(&["test-pinger"])
            .inc();

        encoder
            .encode(&metrics.registry.gather(), &mut buffer)
            .unwrap();

        let rendered = String::from_utf8(buffer).unwrap();
        assert!(rendered.contains("ping_thing_client_confirmation_latency"));
        assert!(rendered.contains("ping_thing_client_slot_latency"));
        assert!(rendered.contains("ping_thing_client_watcher_up"));
        assert!(rendered.contains("ping_thing_client_watcher_fatal_errors_total"));
        assert!(rendered.contains("ping_thing_client_confirmation_channel_closed_total"));
        assert!(rendered.contains("ping_thing_client_transaction_timeouts_total"));
        assert!(rendered.contains("ping_thing_client_transaction_retries_bucket"));
        assert!(rendered.contains(
            "ping_thing_client_transaction_retries_count{pinger_name=\"test-pinger\"} 2"
        ));
        assert!(rendered
            .contains("ping_thing_client_transaction_retries_sum{pinger_name=\"test-pinger\"} 3"));
        assert!(rendered.contains("ping_thing_client_validators_app_send_failures_total"));
    }
}
