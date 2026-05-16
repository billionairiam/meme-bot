use std::fmt;

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BuildMode {
    Template,
    AiFree,
}

impl BuildMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Template => "template",
            Self::AiFree => "ai_free",
        }
    }

    pub fn from_db(value: &str) -> Option<Self> {
        match value {
            "template" => Some(Self::Template),
            "ai_free" => Some(Self::AiFree),
            _ => None,
        }
    }
}

/// Single status field for order lifecycle. Replaces the old payment_status +
/// site_status + generation_jobs.status combination.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrderStatus {
    NeedChain,
    NeedLogo,
    NeedTicker,
    NeedName,
    NeedContract,
    NeedXLink,
    NeedTme,
    NeedDomain,
    NeedPay,
    PaidAndGenerating,
    NeedManualReview,
    Done,
    Cancelled,
}

impl OrderStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NeedChain => "need_chain",
            Self::NeedLogo => "need_logo",
            Self::NeedTicker => "need_ticker",
            Self::NeedName => "need_name",
            Self::NeedContract => "need_contract",
            Self::NeedXLink => "need_x_link",
            Self::NeedTme => "need_tme",
            Self::NeedDomain => "need_domain",
            Self::NeedPay => "need_pay",
            Self::PaidAndGenerating => "paid_and_generating",
            Self::NeedManualReview => "need_manual_review",
            Self::Done => "done",
            Self::Cancelled => "cancelled",
        }
    }

    pub fn from_db(value: &str) -> Option<Self> {
        match value {
            "need_chain" => Some(Self::NeedChain),
            "need_logo" => Some(Self::NeedLogo),
            "need_ticker" => Some(Self::NeedTicker),
            "need_name" => Some(Self::NeedName),
            "need_contract" => Some(Self::NeedContract),
            "need_x_link" => Some(Self::NeedXLink),
            "need_tme" => Some(Self::NeedTme),
            "need_domain" => Some(Self::NeedDomain),
            "need_pay" => Some(Self::NeedPay),
            "paid_and_generating" => Some(Self::PaidAndGenerating),
            "need_manual_review" => Some(Self::NeedManualReview),
            "done" => Some(Self::Done),
            "cancelled" => Some(Self::Cancelled),
            _ => None,
        }
    }

    /// Returns true if this is a wizard step where user is still filling in assets.
    pub fn is_wizard_step(self) -> bool {
        matches!(
            self,
            Self::NeedLogo
                | Self::NeedChain
                | Self::NeedTicker
                | Self::NeedName
                | Self::NeedContract
                | Self::NeedXLink
                | Self::NeedTme
                | Self::NeedDomain
        )
    }

    /// Returns true if user can /continue this order.
    pub fn is_continuable(self) -> bool {
        self.is_wizard_step() || self == Self::NeedPay
    }

    /// Returns true if user can /cancel this order.
    pub fn is_cancellable(self) -> bool {
        self.is_wizard_step() || self == Self::NeedPay
    }

    /// Returns true if this order is "active" (shown in /status).
    pub fn is_active(self) -> bool {
        !matches!(self, Self::Done | Self::Cancelled)
    }

    /// Human-readable label for Telegram display.
    pub fn display_label(self) -> &'static str {
        match self {
            Self::NeedLogo => "📷 Waiting for logo",
            Self::NeedChain => "⛓ Waiting for chain",
            Self::NeedTicker => "🏷 Waiting for ticker",
            Self::NeedName => "📝 Waiting for token name",
            Self::NeedContract => "📄 Waiting for contract address",
            Self::NeedXLink => "🔗 Waiting for X link",
            Self::NeedTme => "💬 Waiting for Telegram link",
            Self::NeedDomain => "🌐 Waiting for domain",
            Self::NeedPay => "💰 Waiting for payment",
            Self::PaidAndGenerating => "⚙️ Generating your website...",
            Self::NeedManualReview => "🔧 Under admin review",
            Self::Done => "✅ Live",
            Self::Cancelled => "❌ Cancelled",
        }
    }
}

impl fmt::Display for OrderStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PaymentChain {
    Erc20,
    Tron,
    Sol,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TokenChain {
    Eth,
    Bsc,
    Sol,
    Ton,
}

impl TokenChain {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Eth => "eth",
            Self::Bsc => "bsc",
            Self::Sol => "sol",
            Self::Ton => "ton",
        }
    }

    pub fn display_label(self) -> &'static str {
        match self {
            Self::Eth => "ETH",
            Self::Bsc => "BSC",
            Self::Sol => "SOL",
            Self::Ton => "TON",
        }
    }

    pub fn from_db(value: &str) -> Option<Self> {
        match value {
            "eth" => Some(Self::Eth),
            "bsc" => Some(Self::Bsc),
            "sol" => Some(Self::Sol),
            "ton" => Some(Self::Ton),
            _ => None,
        }
    }
}

impl PaymentChain {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Erc20 => "erc20",
            Self::Tron => "tron",
            Self::Sol => "sol",
        }
    }

    pub fn from_db(value: &str) -> Option<Self> {
        match value {
            "erc20" => Some(Self::Erc20),
            "tron" => Some(Self::Tron),
            "sol" => Some(Self::Sol),
            _ => None,
        }
    }
}

/// Input data collected during the wizard steps. Stored directly on the order.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WebsiteInput {
    pub token_chain: TokenChain,
    pub token_ticker: String,
    pub token_name: String,
    pub contract_address: String,
    pub x_url: String,
    pub telegram_url: String,
    pub logo_asset_path: String,
    pub requested_domain: Option<String>,
}

#[derive(Clone, Debug)]
pub struct User {
    pub id: Uuid,
    pub telegram_user_id: i64,
    pub telegram_username: Option<String>,
    pub is_group_member: bool,
}

#[derive(Clone, Debug)]
pub struct Order {
    pub id: Uuid,
    pub user_id: Uuid,
    pub status: OrderStatus,
    pub build_mode: Option<BuildMode>,
    pub template_id: Option<String>,
    // wizard assets
    pub token_chain: Option<TokenChain>,
    pub token_ticker: Option<String>,
    pub token_name: Option<String>,
    pub contract_address: Option<String>,
    pub x_url: Option<String>,
    pub telegram_url: Option<String>,
    pub logo_asset_path: Option<String>,
    pub requested_domain: Option<String>,
    pub alternative_domains_json: Option<String>,
    // pricing
    pub site_price_usdt: Decimal,
    pub domain_price_usdt: Decimal,
    pub total_usdt: Decimal,
    // payment
    pub payment_chain: Option<PaymentChain>,
    pub tx_hash: Option<String>,
    pub payment_expires_at: Option<DateTime<Utc>>,
    pub paid_at: Option<DateTime<Utc>>,
    pub pending_contract_address: Option<String>,
    pub pending_x_url: Option<String>,
    pub pending_telegram_url: Option<String>,
    pub pending_edit_field: Option<String>,
    pub edit_status: String,
    pub edit_payment_chain: Option<PaymentChain>,
    pub edit_tx_hash: Option<String>,
    pub edit_payment_expires_at: Option<DateTime<Utc>>,
    pub edit_paid_at: Option<DateTime<Utc>>,
    pub edit_success_count: i64,
    // generation queue
    pub attempt_count: i64,
    // timestamps
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Order {
    /// Build a `WebsiteInput` from the order's wizard fields.
    /// Returns `None` if any required field is missing.
    pub fn website_input(&self) -> Option<WebsiteInput> {
        Some(WebsiteInput {
            token_chain: self.token_chain?,
            token_ticker: self.token_ticker.clone()?,
            token_name: self.token_name.clone()?,
            contract_address: self
                .contract_address
                .clone()
                .unwrap_or_else(|| crate::validation::DEFAULT_CONTRACT_ADDRESS.to_string()),
            x_url: self
                .x_url
                .clone()
                .unwrap_or_else(|| crate::validation::DEFAULT_X_URL.to_string()),
            telegram_url: self
                .telegram_url
                .clone()
                .unwrap_or_else(|| crate::validation::DEFAULT_TELEGRAM_URL.to_string()),
            logo_asset_path: self.logo_asset_path.clone()?,
            requested_domain: Some(self.requested_domain.clone()?),
        })
    }
}

/// A completed website record, created when deployment succeeds.
#[derive(Clone, Debug)]
pub struct Website {
    pub id: Uuid,
    pub user_id: Uuid,
    pub order_id: Uuid,
    pub token_chain: TokenChain,
    pub token_ticker: String,
    pub token_name: String,
    pub contract_address: String,
    pub x_url: String,
    pub telegram_url: String,
    pub logo_asset_path: String,
    pub requested_domain: Option<String>,
    pub generation_provider_used: Option<String>,
    pub github_repo_url: Option<String>,
    pub cloudflare_pages_url: Option<String>,
    pub custom_domain: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Clone, Debug)]
pub struct Template {
    pub template_id: String,
    pub name: String,
    pub local_path: String,
    pub preview_url: Option<String>,
    pub price_tier: Decimal,
    pub enabled: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn website_input_requires_logo_ticker_name_and_domain() {
        let order = Order {
            id: Uuid::new_v4(),
            user_id: Uuid::new_v4(),
            status: OrderStatus::NeedPay,
            build_mode: Some(BuildMode::AiFree),
            template_id: None,
            token_chain: Some(TokenChain::Eth),
            token_ticker: Some("LOGO".to_string()),
            token_name: Some("Logo Coin".to_string()),
            contract_address: None,
            x_url: None,
            telegram_url: None,
            logo_asset_path: Some("/tmp/logo.png".to_string()),
            requested_domain: Some("logocoin.xyz".to_string()),
            alternative_domains_json: None,
            site_price_usdt: Decimal::new(50, 0),
            domain_price_usdt: Decimal::ZERO,
            total_usdt: Decimal::new(50, 0),
            payment_chain: None,
            tx_hash: None,
            payment_expires_at: None,
            paid_at: None,
            pending_contract_address: None,
            pending_x_url: None,
            pending_telegram_url: None,
            pending_edit_field: None,
            edit_status: "none".to_string(),
            edit_payment_chain: None,
            edit_tx_hash: None,
            edit_payment_expires_at: None,
            edit_paid_at: None,
            edit_success_count: 0,
            attempt_count: 0,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };

        let input = order.website_input().expect("required input is valid");
        assert_eq!(input.token_chain, TokenChain::Eth);
        assert_eq!(input.token_ticker, "LOGO");
        assert_eq!(input.token_name, "Logo Coin");
        assert_eq!(input.logo_asset_path, "/tmp/logo.png");
        assert_eq!(
            input.contract_address,
            crate::validation::DEFAULT_CONTRACT_ADDRESS
        );
        assert_eq!(input.x_url, crate::validation::DEFAULT_X_URL);
        assert_eq!(input.telegram_url, crate::validation::DEFAULT_TELEGRAM_URL);
        assert_eq!(input.requested_domain.as_deref(), Some("logocoin.xyz"));
    }
}
