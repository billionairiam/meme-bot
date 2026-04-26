use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use thiserror::Error;
use uuid::Uuid;

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
