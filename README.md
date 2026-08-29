# Ping Thing Client

Ping Thing Client measures how long a Solana transaction takes to land.

[validators.app dashboard](https://www.validators.app/ping-thing?locale=en&network=mainnet)

It builds a small self-transfer transaction, sends it to a Solana endpoint, watches the wallet through Yellowstone gRPC, and records the time when the exact transaction signature is observed. It also records the difference between the slot used when the transaction was created and the slot where it landed.

## How it works

1. The client opens one Yellowstone gRPC connection and clones the client for three subscriptions:
   - Latest blockhashes.
   - Current slots.
   - Successful transactions involving the configured wallet.
2. If dynamic priority fees are enabled, it polls `getRecentPrioritizationFees`, waits for the first result, and keeps the latest successful result cached.
3. It waits for a blockhash that was not used by the previous logical transaction.
4. It creates and signs a 5,000-lamport self-transfer with the configured compute-unit limit and priority fee.
5. It publishes the signed transaction's exact signature to the transaction watcher before sending.
6. It sends the transaction through `RPC_ENDPOINT` or the optional `SEND_TX_ENDPOINT`.
7. If sending fails, it retries immediately. If sending succeeds but the exact signature is not observed, it resends the same signed transaction after `TX_RESEND_INTERVAL_MS`.
8. When the exact signature is observed, it records time and slot latency. Other wallet transactions are ignored.
9. It sends the completed result to Validators.app in a separate task unless that reporting is disabled.

Failed on-chain transactions are intentionally filtered out by the gRPC subscription. A timed-out logical transaction consumes its blockhash; the next logical transaction waits for a new one.

## Requirements

- Rust 1.96.0. The repository toolchain file selects it automatically through `rustup`.
- A Solana RPC endpoint.
- A Yellowstone gRPC endpoint that provides block metadata, slot, and transaction subscriptions.
- A funded Solana wallet. The wallet pays transaction fees.
- A Validators.app API key when Validators.app reporting is enabled.

## Configure

Fill in `.env.local`. Endpoint, API token, and private-key values are intentionally blank.

`dotenv` automatically reads `.env`, not `.env.local`. Export `.env.local` into the shell before running:

```bash
set -a
source .env.local
set +a
```

`SEND_TX_ENDPOINT` needs special care:

- Set it to a JSON-RPC or Triton `/sendtx` endpoint to use a separate send path.
- Remove or unset it to send through `RPC_ENDPOINT`.
- Do not leave it exported as an empty string when running, because an empty value is treated as a configured endpoint.

## Run

Build and run an optimized binary:

```bash
cargo run --release
```

The client waits until its blockhash, slot, and transaction subscriptions are ready before entering the transaction loop. When dynamic priority fees are enabled, it also waits for the first priority-fee response.

Prometheus metrics are available at:

```text
http://0.0.0.0:9090/metrics
```

Use the configured `PROMETHEUS_PORT` when it is not `9090`. For example:

```bash
curl http://127.0.0.1:${PROMETHEUS_PORT}/metrics
```

## Environment variables

| Variable | Template default | Purpose |
| --- | --- | --- |
| `SEND_TX_ENDPOINT` | blank | Optional separate transaction-send endpoint. Unset it to use `RPC_ENDPOINT`. |
| `SEND_TX_KIND` | `json_rpc` | Send endpoint type. Supported values are `json_rpc` and Triton `sendtx` aliases. |
| `RPC_ENDPOINT` | blank | Required Solana JSON-RPC endpoint. It is also used for priority-fee polling and as the default transaction-send endpoint. |
| `WS_ENDPOINT` | blank | Compatibility placeholder. The current client does not read it. |
| `VA_API_KEY` | blank | Validators.app API key. Required for successful reporting. |
| `GRPC_ENDPOINT` | blank | Required Yellowstone gRPC endpoint. |
| `GRPC_X_TOKEN` | blank | Optional Yellowstone gRPC authentication token. |
| `PINGER_NAME` | `UNSET` | Name added to every exported metric as `pinger_name`. Use a unique value such as `n.eu.mainnet`. |
| `PINGER_REGION` | `local` | Region sent in the Validators.app payload. |
| `SKIP_VALIDATORS_APP` | `false` | Set to `true` to skip Validators.app requests. |
| `WALLET_PRIVATE_KEYPAIR` | blank | Required base58-encoded Solana private keypair. Never commit a real value. |
| `SLEEP_MS_RPC` | `350` | Compatibility placeholder. The current client does not read it; priority-fee polling currently uses 350 ms internally. |
| `SLEEP_MS_LOOP` | `0` | Delay in milliseconds before creating the next logical transaction. It does not delay retries. |
| `VERBOSE_LOG` | `false` | Read and printed at startup, but it does not set the log filter. Use `RUST_LOG` for log filtering. |
| `COMMITMENT` | `confirmed` | gRPC commitment: `processed`, `confirmed`, or `finalized`. |
| `CU_BUDGET` | `500` | Compute-unit limit placed in each transaction. |
| `PRIORITY_FEE_MICRO_LAMPORTS` | `5000` | Compatibility placeholder. Dynamic mode uses the latest cached RPC result; this value is not currently read. |
| `PRIORITY_FEE_PERCENTILE` | `5000` | Priority-fee percentile sent to RPC. `5000` represents the 50th percentile. |
| `USE_PRIORITY_FEE` | `false` | Set to `true` to poll and use dynamic priority fees. |
| `PROMETHEUS_PORT` | `9090` | Port used by the `/metrics` HTTP server. |
| `RUST_LOG` | `info` | Log filter used by `env_logger`. |
| `TXS_PER_MINUTE_LIMIT` | `10` | Maximum number of new logical transactions created in each fixed 60-second window. Retries do not increment this count. |
| `TX_RESEND_INTERVAL_MS` | `2000` | Wait after a successful send before resending an unconfirmed transaction. Send errors retry immediately. Must be greater than zero. |
| `LAMPORTS_MULTIPLIER` | `1.0,2.0` | Range with which lamports to transfer are multiplied to ensure unique transactions. |

The program also supports advanced send settings such as `SEND_TX_ENCODING`, `SEND_TX_MAX_RETRIES`, `SEND_TX_RESPONSE_SIGNATURE`, and `SEND_TX_FORWARDING_POLICIES`. It supports `TX_CONFIRMATION_TIMEOUT`, `SKIP_PREFLIGHT`, `SKIP_PROMETHEUS`, `VALIDATORS_APP_REQUEST_TIMEOUT_MS`, and the optional `USE_MEMO_IX_WITH_STRING` memo.

## Exported metrics

Every metric created by the client includes a `pinger_name` label. Histogram metrics export `_bucket`, `_sum`, and `_count` series.

| Metric | Type | Unit | Labels | Meaning |
| --- | --- | --- | --- | --- |
| `ping_thing_client_confirmation_latency` | Histogram | milliseconds | `pinger_name` | Time from the first send attempt to observing the exact signature through gRPC. |
| `ping_thing_client_slot_latency` | Histogram | slots | `pinger_name` | Landed slot minus the slot captured when the transaction was created. Buckets cover 0 through 30 slots, plus `+Inf`. |
| `ping_thing_client_watcher_up` | Gauge | boolean | `pinger_name`, `watcher` | `1` while a blockhash, slot, or transaction watcher is running; otherwise `0`. |
| `ping_thing_client_watcher_fatal_errors_total` | Counter | errors | `pinger_name`, `watcher` | Fatal watcher errors. |
| `ping_thing_client_transaction_timeouts_total` | Counter | transactions | `pinger_name` | Logical transactions not confirmed before the local confirmation timeout. |
| `ping_thing_client_transaction_retries` | Histogram | retry number | `pinger_name` | One observation for every resend. Retry `1` means the transaction was sent for the second time. |
| `ping_thing_client_send_request_duration_ms` | Histogram | milliseconds | `pinger_name`, `send_kind`, `outcome` | Duration of completed transaction-send requests, split by send path and success or failure. |
| `ping_thing_client_blockhash_wait_duration_ms` | Histogram | milliseconds | `pinger_name` | Time spent waiting for a new usable blockhash. |
| `ping_thing_client_priority_fee_cache_age_ms` | Gauge | milliseconds | `pinger_name` | Age of the cached priority-fee response when it was selected for a transaction. |
| `ping_thing_client_validators_app_send_failures_total` | Counter | failures | `pinger_name` | Validators.app requests that failed or timed out. |

Labels such as `job`, `service`, `instance`, and datacenter labels may also appear after Prometheus adds its scrape labels. They are not created by this client.

## Latency meaning

Confirmation latency starts immediately before the first transaction-send request is polled. The initial send log is written before this clock starts. The transaction watcher records `observed_at` only when it sees the exact signature being measured and does so before confirmation logging or Validators.app reporting.

The value therefore measures the client-observed landing delay. Network delay between the validator and the gRPC provider is still part of the observation path.
