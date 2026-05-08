use async_trait::async_trait;
use chrono::{DateTime, TimeZone, Utc};
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
            confirmed_at: Utc::now(),
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

        let observed_amount_usdt = usdt_decimal_from_base_units(&transfer.amount, 6)?;
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
            let confirmed_at = event
                .get("block_timestamp")
                .or_else(|| event.get("timestamp"))
                .and_then(Value::as_i64)
                .and_then(|ms| Utc.timestamp_millis_opt(ms).single())
                .unwrap_or_else(Utc::now);

            return Ok(PaymentVerificationOutcome {
                observed_amount_usdt: usdt_decimal_from_base_units(&raw_amount, 6)?,
                observed_to_address: to.to_string(),
                confirmations: Some(self.tron_min_confirmations),
                confirmed_at,
            });
        }

        Err(PaymentVerificationError::WrongRecipient)
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
    amount: String,
}

fn find_erc20_transfer(
    logs: Option<&Vec<Value>>,
    contract: &str,
    to: &str,
) -> Option<Erc20Transfer> {
    let transfer_topic = "0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef";
    let normalized_contract = normalize_hex(contract);
    let normalized_to = normalize_hex(to);
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
        return Some(Erc20Transfer {
            to: to.to_string(),
            amount: log.get("data")?.as_str()?.to_string(),
        });
    }
    None
}

fn find_solana_usdt_transfer(tx: &Value, mint: &str, recipient: &str) -> Option<Decimal> {
    let instructions = tx
        .pointer("/transaction/message/instructions")
        .and_then(Value::as_array)?;
    for instruction in instructions {
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
        if info
            .get("destination")
            .or_else(|| info.get("destinationOwner"))
            .and_then(Value::as_str)
            != Some(recipient)
        {
            continue;
        }
        if let Some(amount) = info
            .pointer("/tokenAmount/uiAmountString")
            .and_then(Value::as_str)
        {
            return Decimal::from_str_exact(amount).ok();
        }
        if let Some(amount) = info.pointer("/tokenAmount/amount").and_then(Value::as_str) {
            return usdt_decimal_from_base_units(amount, 6).ok();
        }
    }
    None
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
    use chrono::Duration;
    use rust_decimal::Decimal;

    use super::*;

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
            expires_at: Utc::now() - Duration::minutes(1),
        };

        assert_eq!(
            service.verify_tx(request).await.unwrap_err(),
            PaymentVerificationError::ExpiredOrder
        );
    }
}
