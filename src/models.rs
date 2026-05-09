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
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrderType {
    SiteFee,
    DomainFee,
}

impl OrderType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SiteFee => "site_fee",
            Self::DomainFee => "domain_fee",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PaymentChain {
    Erc20,
    Tron,
    Sol,
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

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WebsiteInput {
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
    pub order_type: OrderType,
    pub build_mode: Option<BuildMode>,
    pub template_id: Option<String>,
    pub amount_usdt: Decimal,
    pub payment_chain: Option<PaymentChain>,
    pub payment_status: String,
    pub site_status: String,
    pub payment_started_at: Option<DateTime<Utc>>,
    pub payment_expires_at: Option<DateTime<Utc>>,
    pub paid_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug)]
pub struct Website {
    pub id: Uuid,
    pub user_id: Uuid,
    pub order_id: Uuid,
    pub input: WebsiteInput,
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
