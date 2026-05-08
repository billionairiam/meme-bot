use async_trait::async_trait;
use chrono::{DateTime, Duration, TimeZone, Utc};
use reqwest::Client;
use rust_decimal::Decimal;
use serde_json::{Value, json};
use thiserror::Error;
use uuid::Uuid;

use crate::config::Config;
use crate::models::PaymentChain;

#[derive(Clone, Debug)]
pub struct PaymentVerificationRequest {
    pub order_id: Uuid,
    pub chain: PaymentChain,
    pub tx_hash: String,
    pub expected_to_address: String,
    pub required_amount_usdt: Decimal,
    pub order_created_at: DateTime<Utc>,
    pub submitted_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

#[derive(Clone, Debug)]
pub struct PaymentVerificationOutcome {
    pub observed_amount_usdt: Decimal,
    pub observed_to_address: String,
    pub confirmations: Option<u64>,
    pub confirmed_at: DateTime<Utc>,
}

#[derive(Debug, Error, Clone, Eq, PartialEq)]
pub enum PaymentVerificationError {
    #[error("transaction not found")]
    NotFound,
    #[error("wrong chain")]
    WrongChain,
    #[error("wrong token")]
    WrongToken,
    #[error("wrong recipient")]
    WrongRecipient,
    #[error("insufficient amount")]
    InsufficientAmount,
    #[error("insufficient confirmations")]
    InsufficientConfirmations,
    #[error("transaction already used")]
    AlreadyUsed,
    #[error("payment order expired")]
    ExpiredOrder,
    #[error("transaction is older than order")]
    OldTransaction,
    #[error("transaction timestamp is after user submission")]
    TransactionAfterSubmission,
    #[error("provider error: {0}")]
    Provider(String),
}

#[async_trait]
pub trait PaymentVerifier: Send + Sync {
    async fn verify(
        &self,
        request: &PaymentVerificationRequest,
    ) -> Result<PaymentVerificationOutcome, PaymentVerificationError>;
}

pub struct PaymentService<V> {
    verifier: V,
}

impl<V> PaymentService<V>
where
    V: PaymentVerifier,
{
    pub fn new(verifier: V) -> Self {
        Self { verifier }
    }

    pub async fn verify_tx(
        &self,
        request: PaymentVerificationRequest,
    ) -> Result<PaymentVerificationOutcome, PaymentVerificationError> {
        if Utc::now() > request.expires_at {
            return Err(PaymentVerificationError::ExpiredOrder);
        }

        let outcome = self.verifier.verify(&request).await?;
        if outcome.confirmed_at < request.order_created_at {
            return Err(PaymentVerificationError::OldTransaction);
        }
        if outcome.confirmed_at > request.submitted_at + Duration::minutes(2) {
            return Err(PaymentVerificationError::TransactionAfterSubmission);
        }
        if !outcome
            .observed_to_address
            .eq_ignore_ascii_case(&request.expected_to_address)
        {
            return Err(PaymentVerificationError::WrongRecipient);
        }
        if outcome.observed_amount_usdt < request.required_amount_usdt {
            return Err(PaymentVerificationError::InsufficientAmount);
        }
        Ok(outcome)
    }
}

pub struct StubPaymentVerifier;

#[async_trait]
impl PaymentVerifier for StubPaymentVerifier {
    async fn verify(
        &self,
        request: &PaymentVerificationRequest,
    ) -> Result<PaymentVerificationOutcome, PaymentVerificationError> {
        if !request.tx_hash.starts_with("valid_") {
            return Err(PaymentVerificationError::NotFound);
        }

        Ok(PaymentVerificationOutcome {
            observed_amount_usdt: request.required_amount_usdt,
            observed_to_address: request.expected_to_address.clone(),
            confirmations: Some(match request.chain {
                PaymentChain::Erc20 => 12,
                PaymentChain::Tron => 20,
                PaymentChain::Sol => 1,
            }),
            confirmed_at: request.submitted_at,
        })
    }
}

#[derive(Clone, Debug)]
pub struct ChainPaymentVerifier {
    client: Client,
    erc20_rpc_url: String,
    erc20_contract: String,
    erc20_min_confirmations: u64,
    tron_grid_base_url: String,
    tron_grid_api_key: Option<String>,
    tron_contract: String,
    tron_min_confirmations: u64,
    solana_rpc_url: String,
    sol_mint: String,
    sol_min_confirmations: u64,
}

impl ChainPaymentVerifier {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        erc20_rpc_url: String,
        erc20_contract: String,
        erc20_min_confirmations: u64,
        tron_grid_base_url: String,
        tron_grid_api_key: Option<String>,
        tron_contract: String,
        tron_min_confirmations: u64,
        solana_rpc_url: String,
        sol_mint: String,
        sol_min_confirmations: u64,
    ) -> Self {
        Self {
            client: Client::new(),
            erc20_rpc_url,
            erc20_contract,
            erc20_min_confirmations,
            tron_grid_base_url: tron_grid_base_url.trim_end_matches('/').to_string(),
            tron_grid_api_key,
            tron_contract,
            tron_min_confirmations,
            solana_rpc_url,
            sol_mint,
            sol_min_confirmations,
        }
    }

    pub fn from_config(config: &Config) -> Self {
        Self::new(
            config.usdt_erc20_rpc_url.clone(),
            config.usdt_erc20_contract.clone(),
            config.usdt_erc20_min_confirmations,
            config.tron_grid_base_url.clone(),
            config.tron_grid_api_key.clone(),
            config.usdt_tron_contract.clone(),
            config.usdt_tron_min_confirmations,
            config.solana_rpc_url.clone(),
            config.usdt_sol_mint.clone(),
            config.usdt_sol_min_confirmations,
        )
    }

    async fn verify_erc20(
        &self,
        request: &PaymentVerificationRequest,
    ) -> Result<PaymentVerificationOutcome, PaymentVerificationError> {
        if self.erc20_rpc_url.trim().is_empty() || self.erc20_contract.trim().is_empty() {
            return Err(PaymentVerificationError::Provider(
                "USDT_ERC20_RPC_URL and USDT_ERC20_CONTRACT are required".to_string(),
            ));
        }

        let receipt = self
            .evm_rpc(
                "eth_getTransactionReceipt",
                json!([request.tx_hash.clone()]),
            )
            .await?;
        let Some(receipt) = receipt.as_object() else {
            return Err(PaymentVerificationError::NotFound);
        };
        if receipt.get("status").and_then(Value::as_str) != Some("0x1") {
            return Err(PaymentVerificationError::NotFound);
        }

        let transfer = find_erc20_transfer(
            receipt.get("logs").and_then(Value::as_array),
            &self.erc20_contract,
            &request.expected_to_address,
        )
        .ok_or(PaymentVerificationError::WrongRecipient)?;

        let observed_amount_usdt = transfer.amount;
        let block_number = receipt
            .get("blockNumber")
            .and_then(Value::as_str)
            .and_then(hex_to_u64)
            .ok_or_else(|| PaymentVerificationError::Provider("missing blockNumber".to_string()))?;
        let latest = self
            .evm_rpc("eth_blockNumber", json!([]))
            .await?
            .as_str()
            .and_then(hex_to_u64)
            .ok_or_else(|| {
                PaymentVerificationError::Provider("missing latest block".to_string())
            })?;
        let confirmations = latest.saturating_sub(block_number).saturating_add(1);
        if confirmations < self.erc20_min_confirmations {
            return Err(PaymentVerificationError::InsufficientConfirmations);
        }

        let block = self
            .evm_rpc(
                "eth_getBlockByNumber",
                json!([format!("0x{block_number:x}"), false]),
            )
            .await?;
        let confirmed_at = block
            .get("timestamp")
            .and_then(Value::as_str)
            .and_then(hex_to_u64)
            .and_then(|ts| Utc.timestamp_opt(ts as i64, 0).single())
            .unwrap_or_else(Utc::now);

        Ok(PaymentVerificationOutcome {
            observed_amount_usdt,
            observed_to_address: transfer.to,
            confirmations: Some(confirmations),
            confirmed_at,
        })
    }

    async fn evm_rpc(
        &self,
        method: &str,
        params: Value,
    ) -> Result<Value, PaymentVerificationError> {
        let response = self
            .client
            .post(&self.erc20_rpc_url)
            .json(&json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}))
            .send()
            .await
            .map_err(|e| PaymentVerificationError::Provider(e.to_string()))?;
        let body: Value = response
            .json()
            .await
            .map_err(|e| PaymentVerificationError::Provider(e.to_string()))?;
        if let Some(error) = body.get("error") {
            return Err(PaymentVerificationError::Provider(error.to_string()));
        }
        Ok(body.get("result").cloned().unwrap_or(Value::Null))
    }

    async fn verify_tron(
        &self,
        request: &PaymentVerificationRequest,
    ) -> Result<PaymentVerificationOutcome, PaymentVerificationError> {
        if self.tron_grid_base_url.trim().is_empty() || self.tron_contract.trim().is_empty() {
            return Err(PaymentVerificationError::Provider(
                "TRON_GRID_BASE_URL and USDT_TRON_CONTRACT are required".to_string(),
            ));
        }

        let url = format!(
            "{}/v1/transactions/{}/events?event_name=Transfer&contract_address={}",
            self.tron_grid_base_url, request.tx_hash, self.tron_contract
        );
        let mut builder = self.client.get(url);
        if let Some(api_key) = &self.tron_grid_api_key {
            builder = builder.header("TRON-PRO-API-KEY", api_key);
        }
        let body: Value = builder
            .send()
            .await
            .map_err(|e| PaymentVerificationError::Provider(e.to_string()))?
            .json()
            .await
            .map_err(|e| PaymentVerificationError::Provider(e.to_string()))?;
        let events = body
            .get("data")
            .and_then(Value::as_array)
            .ok_or(PaymentVerificationError::NotFound)?;

        let mut total = Decimal::ZERO;
        let mut observed_to = None;
        let mut block_number = None;
        let mut confirmed_at = None;
        for event in events {
            let result = event.get("result").unwrap_or(event);
            let to = result
                .get("to")
                .or_else(|| result.get("_to"))
                .and_then(Value::as_str)
                .unwrap_or_default();
            if !to.eq_ignore_ascii_case(&request.expected_to_address) {
                continue;
            }
            let raw_amount = result
                .get("value")
                .or_else(|| result.get("_value"))
                .and_then(value_to_decimal_string)
                .ok_or_else(|| {
                    PaymentVerificationError::Provider("missing TRC20 amount".to_string())
                })?;
            total += usdt_decimal_from_base_units(&raw_amount, 6)?;
            observed_to = Some(to.to_string());
            block_number = event
                .get("block_number")
                .or_else(|| event.get("blockNumber"))
                .and_then(value_to_u64)
                .or(block_number);
            confirmed_at = event
                .get("block_timestamp")
                .or_else(|| event.get("timestamp"))
                .and_then(Value::as_i64)
                .and_then(|ms| Utc.timestamp_millis_opt(ms).single())
                .or(confirmed_at);
        }

        let Some(observed_to) = observed_to else {
            return Err(PaymentVerificationError::WrongRecipient);
        };
        let block_number = block_number.ok_or_else(|| {
            PaymentVerificationError::Provider("missing TRON block number".to_string())
        })?;
        let latest = self.tron_latest_block_number().await?;
        let confirmations = latest.saturating_sub(block_number).saturating_add(1);
        if confirmations < self.tron_min_confirmations {
            return Err(PaymentVerificationError::InsufficientConfirmations);
        }

        Ok(PaymentVerificationOutcome {
            observed_amount_usdt: total,
            observed_to_address: observed_to,
            confirmations: Some(confirmations),
            confirmed_at: confirmed_at.unwrap_or_else(Utc::now),
        })
    }

    async fn tron_latest_block_number(&self) -> Result<u64, PaymentVerificationError> {
        let url = format!("{}/wallet/getnowblock", self.tron_grid_base_url);
        let mut builder = self.client.post(url).json(&json!({}));
        if let Some(api_key) = &self.tron_grid_api_key {
            builder = builder.header("TRON-PRO-API-KEY", api_key);
        }
        let body: Value = builder
            .send()
            .await
            .map_err(|e| PaymentVerificationError::Provider(e.to_string()))?
            .json()
            .await
            .map_err(|e| PaymentVerificationError::Provider(e.to_string()))?;
        body.pointer("/block_header/raw_data/number")
            .and_then(Value::as_u64)
            .ok_or_else(|| {
                PaymentVerificationError::Provider("missing latest TRON block".to_string())
            })
    }

    async fn verify_sol(
        &self,
        request: &PaymentVerificationRequest,
    ) -> Result<PaymentVerificationOutcome, PaymentVerificationError> {
        if self.solana_rpc_url.trim().is_empty() || self.sol_mint.trim().is_empty() {
            return Err(PaymentVerificationError::Provider(
                "SOLANA_RPC_URL and USDT_SOL_MINT are required".to_string(),
            ));
        }

        let response = self
            .client
            .post(&self.solana_rpc_url)
            .json(&json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "getTransaction",
                "params": [
                    request.tx_hash.clone(),
                    {"encoding": "jsonParsed", "maxSupportedTransactionVersion": 0}
                ]
            }))
            .send()
            .await
            .map_err(|e| PaymentVerificationError::Provider(e.to_string()))?;
        let body: Value = response
            .json()
            .await
            .map_err(|e| PaymentVerificationError::Provider(e.to_string()))?;
        if let Some(error) = body.get("error") {
            return Err(PaymentVerificationError::Provider(error.to_string()));
        }
        let tx = body
            .get("result")
            .filter(|v| !v.is_null())
            .ok_or(PaymentVerificationError::NotFound)?;
        if !tx.pointer("/meta/err").is_none_or(Value::is_null) {
            return Err(PaymentVerificationError::NotFound);
        }
        let slot = tx.get("slot").and_then(Value::as_u64).unwrap_or_default();
        let block_time = tx
            .get("blockTime")
            .and_then(Value::as_i64)
            .and_then(|ts| Utc.timestamp_opt(ts, 0).single())
            .unwrap_or_else(Utc::now);
        let latest_slot = self.solana_slot().await?;
        let confirmations = latest_slot.saturating_sub(slot).saturating_add(1);
        if confirmations < self.sol_min_confirmations {
            return Err(PaymentVerificationError::InsufficientConfirmations);
        }

        if let Some(amount) =
            find_solana_usdt_transfer(tx, &self.sol_mint, &request.expected_to_address)
        {
            return Ok(PaymentVerificationOutcome {
                observed_amount_usdt: amount,
                observed_to_address: request.expected_to_address.clone(),
                confirmations: Some(confirmations),
                confirmed_at: block_time,
            });
        }

        Err(PaymentVerificationError::WrongRecipient)
    }

    async fn solana_slot(&self) -> Result<u64, PaymentVerificationError> {
        let body: Value = self
            .client
            .post(&self.solana_rpc_url)
            .json(&json!({"jsonrpc": "2.0", "id": 1, "method": "getSlot"}))
            .send()
            .await
            .map_err(|e| PaymentVerificationError::Provider(e.to_string()))?
            .json()
            .await
            .map_err(|e| PaymentVerificationError::Provider(e.to_string()))?;
        body.get("result")
            .and_then(Value::as_u64)
            .ok_or_else(|| PaymentVerificationError::Provider("missing Solana slot".to_string()))
    }
}

#[async_trait]
impl PaymentVerifier for ChainPaymentVerifier {
    async fn verify(
        &self,
        request: &PaymentVerificationRequest,
    ) -> Result<PaymentVerificationOutcome, PaymentVerificationError> {
        match request.chain {
            PaymentChain::Erc20 => self.verify_erc20(request).await,
            PaymentChain::Tron => self.verify_tron(request).await,
            PaymentChain::Sol => self.verify_sol(request).await,
        }
    }
}

struct Erc20Transfer {
    to: String,
    amount: Decimal,
}

fn find_erc20_transfer(
    logs: Option<&Vec<Value>>,
    contract: &str,
    to: &str,
) -> Option<Erc20Transfer> {
    let transfer_topic = "0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef";
    let normalized_contract = normalize_hex(contract);
    let normalized_to = normalize_hex(to);
    let mut total = Decimal::ZERO;
    let mut observed_to = None;
    for log in logs? {
        if normalize_hex(log.get("address")?.as_str()?) != normalized_contract {
            continue;
        }
        let topics = log.get("topics")?.as_array()?;
        if topics.first()?.as_str()? != transfer_topic || topics.len() < 3 {
            continue;
        }
        let topic_to = topics.get(2)?.as_str()?;
        if !topic_to.to_ascii_lowercase().ends_with(&normalized_to) {
            continue;
        }
        let amount = usdt_decimal_from_base_units(log.get("data")?.as_str()?, 6).ok()?;
        total += amount;
        observed_to = Some(to.to_string());
    }
    observed_to.map(|to| Erc20Transfer { to, amount: total })
}

fn find_solana_usdt_transfer(tx: &Value, mint: &str, recipient: &str) -> Option<Decimal> {
    let mut total = Decimal::ZERO;
    let mut found = false;
    for instruction in solana_instructions(tx) {
        let Some(parsed) = instruction.get("parsed") else {
            continue;
        };
        let Some(info) = parsed.get("info") else {
            continue;
        };
        let kind = parsed
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if kind != "transferChecked" && kind != "transfer" {
            continue;
        }
        if info.get("mint").and_then(Value::as_str) != Some(mint) {
            continue;
        }
        if !solana_transfer_matches_recipient(tx, info, mint, recipient) {
            continue;
        }
        if let Some(amount) = info
            .pointer("/tokenAmount/uiAmountString")
            .and_then(Value::as_str)
        {
            total += Decimal::from_str_exact(amount).ok()?;
            found = true;
            continue;
        }
        if let Some(amount) = info.pointer("/tokenAmount/amount").and_then(Value::as_str) {
            total += usdt_decimal_from_base_units(amount, 6).ok()?;
            found = true;
        }
    }
    found.then_some(total)
}

fn solana_instructions(tx: &Value) -> Vec<&Value> {
    let mut instructions = Vec::new();
    if let Some(top_level) = tx
        .pointer("/transaction/message/instructions")
        .and_then(Value::as_array)
    {
        instructions.extend(top_level);
    }
    if let Some(inner_groups) = tx
        .pointer("/meta/innerInstructions")
        .and_then(Value::as_array)
    {
        for group in inner_groups {
            if let Some(inner) = group.get("instructions").and_then(Value::as_array) {
                instructions.extend(inner);
            }
        }
    }
    instructions
}

fn solana_transfer_matches_recipient(
    tx: &Value,
    info: &Value,
    mint: &str,
    recipient: &str,
) -> bool {
    if info
        .get("destinationOwner")
        .and_then(Value::as_str)
        .is_some_and(|owner| owner == recipient)
    {
        return true;
    }

    let Some(destination) = info.get("destination").and_then(Value::as_str) else {
        return false;
    };
    if destination == recipient {
        return true;
    }

    solana_token_account_owner(tx, destination, mint)
        .as_deref()
        .is_some_and(|owner| owner == recipient)
}

fn solana_token_account_owner(tx: &Value, token_account: &str, mint: &str) -> Option<String> {
    let account_index = tx
        .pointer("/transaction/message/accountKeys")
        .and_then(Value::as_array)?
        .iter()
        .position(|account| {
            account
                .get("pubkey")
                .or_else(|| account.get("account"))
                .and_then(Value::as_str)
                == Some(token_account)
        })? as u64;

    tx.pointer("/meta/postTokenBalances")
        .and_then(Value::as_array)?
        .iter()
        .find(|balance| {
            balance.get("accountIndex").and_then(Value::as_u64) == Some(account_index)
                && balance.get("mint").and_then(Value::as_str) == Some(mint)
        })?
        .get("owner")
        .and_then(Value::as_str)
        .map(String::from)
}

fn normalize_hex(value: &str) -> String {
    value.trim_start_matches("0x").to_ascii_lowercase()
}

fn hex_to_u64(value: &str) -> Option<u64> {
    u64::from_str_radix(value.trim_start_matches("0x"), 16).ok()
}

fn value_to_decimal_string(value: &Value) -> Option<String> {
    value
        .as_str()
        .map(String::from)
        .or_else(|| value.as_u64().map(|number| number.to_string()))
}

fn value_to_u64(value: &Value) -> Option<u64> {
    value
        .as_u64()
        .or_else(|| value.as_str().and_then(|number| number.parse().ok()))
}

fn usdt_decimal_from_base_units(
    value: &str,
    decimals: u32,
) -> Result<Decimal, PaymentVerificationError> {
    let normalized = value.trim_start_matches("0x");
    let integer = if value.starts_with("0x") {
        u128::from_str_radix(normalized, 16)
            .map_err(|e| PaymentVerificationError::Provider(format!("parse amount: {e}")))?
    } else {
        normalized
            .parse::<u128>()
            .map_err(|e| PaymentVerificationError::Provider(format!("parse amount: {e}")))?
    };
    if integer > i128::MAX as u128 {
        return Err(PaymentVerificationError::Provider(
            "amount is too large".to_string(),
        ));
    }
    Ok(Decimal::from_i128_with_scale(integer as i128, decimals).normalize())
}

#[cfg(test)]
mod tests {
    use rust_decimal::Decimal;

    use super::*;

    struct FixedTimeVerifier {
        confirmed_at: DateTime<Utc>,
    }

    #[async_trait]
    impl PaymentVerifier for FixedTimeVerifier {
        async fn verify(
            &self,
            request: &PaymentVerificationRequest,
        ) -> Result<PaymentVerificationOutcome, PaymentVerificationError> {
            Ok(PaymentVerificationOutcome {
                observed_amount_usdt: request.required_amount_usdt,
                observed_to_address: request.expected_to_address.clone(),
                confirmations: Some(12),
                confirmed_at: self.confirmed_at,
            })
        }
    }

    #[tokio::test]
    async fn allows_overpayment_or_exact_payment() {
        let service = PaymentService::new(StubPaymentVerifier);
        let request = PaymentVerificationRequest {
            order_id: Uuid::new_v4(),
            chain: PaymentChain::Erc20,
            tx_hash: "valid_0xabc".to_string(),
            expected_to_address: "0x1111111111111111111111111111111111111111".to_string(),
            required_amount_usdt: Decimal::new(50, 0),
            order_created_at: Utc::now() - Duration::minutes(1),
            submitted_at: Utc::now(),
            expires_at: Utc::now() + Duration::minutes(29),
        };

        assert!(service.verify_tx(request).await.is_ok());
    }

    #[tokio::test]
    async fn rejects_expired_orders() {
        let service = PaymentService::new(StubPaymentVerifier);
        let request = PaymentVerificationRequest {
            order_id: Uuid::new_v4(),
            chain: PaymentChain::Tron,
            tx_hash: "valid_tx".to_string(),
            expected_to_address: "T111111111111111111111111111111111".to_string(),
            required_amount_usdt: Decimal::new(30, 0),
            order_created_at: Utc::now() - Duration::hours(1),
            submitted_at: Utc::now(),
            expires_at: Utc::now() - Duration::minutes(1),
        };

        assert_eq!(
            service.verify_tx(request).await.unwrap_err(),
            PaymentVerificationError::ExpiredOrder
        );
    }

    #[tokio::test]
    async fn rejects_transactions_confirmed_well_after_user_submission() {
        let submitted_at = Utc::now();
        let service = PaymentService::new(FixedTimeVerifier {
            confirmed_at: submitted_at + Duration::minutes(3),
        });
        let request = PaymentVerificationRequest {
            order_id: Uuid::new_v4(),
            chain: PaymentChain::Erc20,
            tx_hash: "valid_0xabc".to_string(),
            expected_to_address: "0x1111111111111111111111111111111111111111".to_string(),
            required_amount_usdt: Decimal::new(50, 0),
            order_created_at: submitted_at - Duration::minutes(1),
            submitted_at,
            expires_at: submitted_at + Duration::minutes(29),
        };

        assert_eq!(
            service.verify_tx(request).await.unwrap_err(),
            PaymentVerificationError::TransactionAfterSubmission
        );
    }

    #[tokio::test]
    async fn allows_transactions_within_clock_skew_tolerance() {
        let submitted_at = Utc::now();
        let service = PaymentService::new(FixedTimeVerifier {
            confirmed_at: submitted_at + Duration::seconds(30),
        });
        let request = PaymentVerificationRequest {
            order_id: Uuid::new_v4(),
            chain: PaymentChain::Erc20,
            tx_hash: "valid_0xabc".to_string(),
            expected_to_address: "0x1111111111111111111111111111111111111111".to_string(),
            required_amount_usdt: Decimal::new(50, 0),
            order_created_at: submitted_at - Duration::minutes(1),
            submitted_at,
            expires_at: submitted_at + Duration::minutes(29),
        };

        assert!(service.verify_tx(request).await.is_ok());
    }
}
