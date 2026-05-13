use async_trait::async_trait;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// TLDs offered in the Telegram UI for alternative-domain suggestions.
/// Not a validation whitelist; Namecheap decides what it supports.
pub const SUGGESTED_TLDS: &[&str] = &[
    "com", "net", "org", "xyz", "fun", "site", "online", "io", "co", "me", "cc", "app", "dev",
    "world", "space", "club", "store", "shop", "vip",
];

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DomainQuoteRequest {
    pub domain: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DomainQuote {
    pub domain: String,
    pub tld: String,
    pub available: bool,
    pub is_premium: bool,
    pub price_usd: Decimal,
    pub years: i32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DomainPurchaseRequest {
    pub domain: String,
    pub years: i32,
    pub privacy_enabled: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DomainPurchaseResult {
    pub namecheap_order_id: String,
    pub namecheap_transaction_id: String,
    pub charged_amount_usd: Decimal,
}

#[derive(Debug, Error)]
pub enum DomainError {
    #[error("unsupported tld")]
    UnsupportedTld,
    #[error("domain unavailable")]
    Unavailable,
    #[error(
        "domain appears to be registered in the Namecheap account; manual reconciliation required"
    )]
    AlreadyRegisteredToAccount,
    #[error("premium or EAP domain requires manual handling")]
    PremiumRejected,
    #[error("missing Namecheap configuration: {0}")]
    MissingConfig(String),
    #[error("namecheap http error: {0}")]
    Http(String),
    #[error("namecheap parse error: {0}")]
    Parse(String),
    #[error("namecheap error: {0}")]
    Namecheap(String),
    #[error("cloudflare dns error: {0}")]
    Cloudflare(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiErrorResponse {
    pub error: String,
    pub message: String,
}

#[async_trait]
pub trait DomainRegistrar: Send + Sync {
    async fn quote(&self, request: &DomainQuoteRequest) -> Result<DomainQuote, DomainError>;

    async fn purchase(
        &self,
        request: &DomainPurchaseRequest,
    ) -> Result<DomainPurchaseResult, DomainError>;
}
