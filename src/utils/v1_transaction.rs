use anyhow::{ensure, Context, Result};
use solana_client::{
    nonblocking::rpc_client::RpcClient, rpc_config::RpcSimulateTransactionConfig,
    rpc_response::RpcSimulateTransactionResult,
};
use solana_hash::Hash;
use solana_keypair::Keypair;
use solana_message::{v1, VersionedMessage};
use solana_signer::Signer;
use solana_system_interface::instruction::transfer;
use solana_transaction::versioned::VersionedTransaction;
use solana_transaction_status::UiTransactionEncoding;
use spl_memo_interface::{instruction::build_memo, v3};

pub const MAX_TRANSACTION_BYTES: usize = 4096;
pub const MAX_COMPUTE_UNITS: u32 = 1_400_000;
pub const MAX_ACCOUNT_BYTES: u32 = 64 * 1024 * 1024;
const ACCOUNT_PAGE_BYTES: u64 = 32 * 1024;

pub struct PreparedTransaction {
    pub transaction: VersionedTransaction,
    pub bytes: Vec<u8>,
    pub memo_bytes: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResourceLimits {
    pub compute_units: u32,
    pub account_bytes: u32,
}

impl ResourceLimits {
    pub fn config(self, priority_fee: u64) -> v1::TransactionConfig {
        v1::TransactionConfig::empty()
            .with_compute_unit_limit(self.compute_units)
            .with_loaded_accounts_data_size_limit(self.account_bytes)
            .with_priority_fee(priority_fee)
    }
}

pub fn parse_limit(value: Option<&str>, name: &str, maximum: u32) -> Result<Option<u32>> {
    value
        .map(|value| {
            let limit = value
                .parse::<u32>()
                .with_context(|| format!("invalid {name}"))?;
            ensure!(
                (1..=maximum).contains(&limit),
                "{name} must be between 1 and {maximum}"
            );
            Ok(limit)
        })
        .transpose()
}

pub fn limit_from_environment(name: &str, maximum: u32) -> Result<Option<u32>> {
    match std::env::var(name) {
        Ok(value) => parse_limit(Some(&value), name, maximum),
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(error) => Err(error).with_context(|| format!("invalid {name}")),
    }
}

pub fn priority_fee_lamports(price: u64, compute_units: u32) -> Result<u64> {
    let fee = (u128::from(price) * u128::from(compute_units)).div_ceil(1_000_000);
    u64::try_from(fee).context("total priority fee exceeds u64 lamports")
}

fn sign(
    payer: &Keypair,
    blockhash: Hash,
    lamports: u64,
    memo: Option<&str>,
    config: v1::TransactionConfig,
) -> Result<PreparedTransaction> {
    let address = payer.pubkey();
    let mut instructions = vec![transfer(&address, &address, lamports)];
    if let Some(memo) = memo {
        instructions.push(build_memo(&v3::id(), memo.as_bytes(), &[&address]));
    }
    let message = v1::Message::try_compile_with_config(&address, &instructions, blockhash, config)?;
    let transaction = VersionedTransaction::try_new(VersionedMessage::V1(message), &[payer])?;
    let bytes = wincode::serialize(&transaction).context("serialize v1 transaction")?;
    Ok(PreparedTransaction {
        transaction,
        bytes,
        memo_bytes: memo.map_or(0, str::len),
    })
}

pub fn build(
    payer: &Keypair,
    blockhash: Hash,
    lamports: u64,
    memo: Option<&str>,
    limits: ResourceLimits,
    priority_fee: u64,
) -> Result<PreparedTransaction> {
    let memo = memo.filter(|text| !text.is_empty());
    ensure!(
        memo.map_or(0, str::len) <= MAX_TRANSACTION_BYTES,
        "memo exceeds transaction size limit"
    );
    let config = limits.config(priority_fee);
    let candidate = sign(payer, blockhash, lamports, memo, config)?;
    let remaining = MAX_TRANSACTION_BYTES
        .checked_sub(candidate.bytes.len())
        .context("memo does not fit in a 4096-byte signed transaction")?;
    let Some(memo) = memo else {
        return Ok(candidate);
    };
    let padded = format!("{memo}{}", " ".repeat(remaining));
    let prepared = sign(payer, blockhash, lamports, Some(&padded), config)?;
    ensure!(
        prepared.bytes.len() == MAX_TRANSACTION_BYTES,
        "padded transaction must be exactly 4096 bytes"
    );
    Ok(prepared)
}

fn selected_limit(
    measured: u64,
    estimated: u64,
    explicit: Option<u32>,
    maximum: u32,
    name: &str,
) -> Result<u32> {
    let selected = explicit.map_or(estimated, u64::from);
    ensure!(
        selected >= measured,
        "{name} is below simulated usage ({measured})"
    );
    ensure!(
        (1..=u64::from(maximum)).contains(&selected),
        "{name} with headroom exceeds supported bounds"
    );
    Ok(u32::try_from(selected)?)
}

fn simulation_usage(result: &RpcSimulateTransactionResult) -> Result<(u64, u64)> {
    ensure!(
        result.err.is_none(),
        "v1 simulation failed: {:?}; logs: {:?}",
        result.err,
        result.logs
    );
    let compute = result
        .units_consumed
        .context("simulation omitted unitsConsumed")?;
    let accounts = result
        .loaded_accounts_data_size
        .context("simulation omitted loadedAccountsDataSize")?;
    Ok((compute, u64::from(accounts)))
}

fn select_limits(
    usage: (u64, u64),
    compute_override: Option<u32>,
    account_override: Option<u32>,
) -> Result<ResourceLimits> {
    let (compute, accounts) = usage;
    let compute_estimate = compute
        .checked_mul(120)
        .context("compute estimate overflow")?
        .div_ceil(100)
        .max(1);
    let account_estimate = accounts.div_ceil(ACCOUNT_PAGE_BYTES).max(1) * ACCOUNT_PAGE_BYTES;
    Ok(ResourceLimits {
        compute_units: selected_limit(
            compute,
            compute_estimate,
            compute_override,
            MAX_COMPUTE_UNITS,
            "CU_BUDGET",
        )?,
        account_bytes: selected_limit(
            accounts,
            account_estimate,
            account_override,
            MAX_ACCOUNT_BYTES,
            "LOADED_ACCOUNTS_DATA_SIZE_LIMIT",
        )?,
    })
}

async fn simulate(client: &RpcClient, transaction: &VersionedTransaction) -> Result<(u64, u64)> {
    let result = client
        .simulate_transaction_with_config(
            transaction,
            RpcSimulateTransactionConfig {
                encoding: Some(UiTransactionEncoding::Base64),
                sig_verify: true,
                ..Default::default()
            },
        )
        .await
        .context("simulate v1 transaction; check RPC support and wallet funding")?;
    simulation_usage(&result.value)
}

pub async fn estimate_limits(
    client: &RpcClient,
    payer: &Keypair,
    memo: Option<&str>,
    compute_override: Option<u32>,
    account_override: Option<u32>,
) -> Result<ResourceLimits> {
    let maximum = ResourceLimits {
        compute_units: MAX_COMPUTE_UNITS,
        account_bytes: MAX_ACCOUNT_BYTES,
    };
    let blockhash = client.get_latest_blockhash().await?;
    let candidate = build(payer, blockhash, 5000, memo, maximum, 0)?;
    let usage = simulate(client, &candidate.transaction).await?;
    let limits = select_limits(usage, compute_override, account_override)?;
    let blockhash = client.get_latest_blockhash().await?;
    let final_transaction = build(payer, blockhash, 5000, memo, limits, 0)?;
    simulate(client, &final_transaction.transaction).await?;
    Ok(limits)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limits() -> ResourceLimits {
        ResourceLimits {
            compute_units: 100_000,
            account_bytes: 32_768,
        }
    }

    fn transaction(memo: Option<&str>, fee: u64) -> Result<PreparedTransaction> {
        build(
            &Keypair::new(),
            Hash::new_from_array([3; 32]),
            5000,
            memo,
            limits(),
            fee,
        )
    }

    #[test]
    fn signed_v1_round_trip_and_config() {
        let tx = transaction(None, 3).unwrap();
        assert_eq!(tx.bytes[0], 0x81);
        let decoded: VersionedTransaction = wincode::deserialize(&tx.bytes).unwrap();
        assert_eq!(decoded, tx.transaction);
        assert_eq!(decoded.verify_with_results(), vec![true]);
        let VersionedMessage::V1(message) = decoded.message else {
            panic!("expected v1")
        };
        assert_eq!(message.config, limits().config(3));
        assert_eq!(message.instructions.len(), 1);
        let unique: std::collections::HashSet<_> = message.account_keys.iter().collect();
        assert_eq!(unique.len(), message.account_keys.len());
    }

    #[test]
    fn memo_padding_preserves_utf8_and_fee_size() {
        for memo in ["ping", "é 🦀"] {
            for fee in [0, 1, u64::MAX] {
                let tx = transaction(Some(memo), fee).unwrap();
                assert_eq!(tx.bytes.len(), MAX_TRANSACTION_BYTES);
                assert_eq!(tx.transaction.verify_with_results(), vec![true]);
                let VersionedMessage::V1(message) = tx.transaction.message else {
                    panic!("expected v1")
                };
                assert_eq!(message.instructions.len(), 2);
                let data = &message.instructions[1].data;
                assert!(data.starts_with(memo.as_bytes()));
                assert!(data[memo.len()..].iter().all(|byte| *byte == b' '));
                assert!(std::str::from_utf8(data).is_ok());
            }
        }
    }

    #[test]
    fn memo_boundaries() {
        assert_eq!(
            transaction(None, 0).unwrap().bytes.len(),
            transaction(Some(""), 0).unwrap().bytes.len()
        );
        let capacity = transaction(Some("x"), 0).unwrap().memo_bytes;
        assert_eq!(
            transaction(Some(&"x".repeat(capacity)), 0)
                .unwrap()
                .bytes
                .len(),
            MAX_TRANSACTION_BYTES
        );
        assert!(transaction(Some(&"x".repeat(capacity + 1)), 0).is_err());
        assert!(transaction(Some(&"x".repeat(MAX_TRANSACTION_BYTES + 1)), 0).is_err());
    }

    #[test]
    fn fee_rounding_and_overflow() {
        assert_eq!(priority_fee_lamports(0, 500).unwrap(), 0);
        assert_eq!(priority_fee_lamports(5000, 500).unwrap(), 3);
        assert_eq!(priority_fee_lamports(2000, 500).unwrap(), 1);
        assert_eq!(
            priority_fee_lamports(u64::MAX, 1_000_000).unwrap(),
            u64::MAX
        );
        assert!(priority_fee_lamports(u64::MAX, MAX_COMPUTE_UNITS).is_err());
    }

    #[test]
    fn override_validation() {
        assert_eq!(parse_limit(None, "limit", 100).unwrap(), None);
        assert_eq!(parse_limit(Some("100"), "limit", 100).unwrap(), Some(100));
        for invalid in ["", "abc", "0", "-1", "101", "4294967296"] {
            assert!(parse_limit(Some(invalid), "limit", 100).is_err());
        }
    }

    #[test]
    fn estimates_headroom_pages_and_bounds() {
        assert_eq!(
            select_limits((101, 32_769), None, None).unwrap(),
            ResourceLimits {
                compute_units: 122,
                account_bytes: 65_536
            }
        );
        assert_eq!(
            select_limits((0, 0), None, None).unwrap(),
            ResourceLimits {
                compute_units: 1,
                account_bytes: 32_768
            }
        );
        assert_eq!(
            select_limits((100, 32_768), Some(100), None)
                .unwrap()
                .account_bytes,
            32_768
        );
        assert!(select_limits((100, 100), Some(99), None).is_err());
        assert!(select_limits((100, 100), None, Some(99)).is_err());
        assert!(select_limits((u64::from(MAX_COMPUTE_UNITS), 100), None, None).is_err());
        assert!(select_limits((u64::MAX, 100), None, None).is_err());
        assert!(select_limits((100, u64::from(MAX_ACCOUNT_BYTES) + 1), None, None).is_err());
    }

    #[test]
    fn simulation_requires_success_and_both_measurements() {
        let mut result: RpcSimulateTransactionResult = serde_json::from_value(serde_json::json!({
            "err": null, "logs": [], "accounts": null, "unitsConsumed": 100,
            "loadedAccountsDataSize": 200, "returnData": null, "innerInstructions": null
        }))
        .unwrap();
        assert_eq!(simulation_usage(&result).unwrap(), (100, 200));
        result.units_consumed = None;
        assert!(simulation_usage(&result).is_err());
        result.units_consumed = Some(100);
        result.loaded_accounts_data_size = None;
        assert!(simulation_usage(&result).is_err());
        result.loaded_accounts_data_size = Some(200);
        result.err = Some(serde_json::from_value(serde_json::json!("AccountNotFound")).unwrap());
        assert!(simulation_usage(&result).is_err());
    }
}
