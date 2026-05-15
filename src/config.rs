use std::env;

use anyhow::{Context, Result};
use rust_decimal::Decimal;

pub fn load_env_file(env_var_name: &str, default_path: &str) -> Result<Option<String>> {
    let path = env::var(env_var_name)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| default_path.to_string());
    match dotenvy::from_filename(&path) {
        Ok(_) => Ok(Some(path)),
        Err(error) if error.not_found() => Ok(None),
        Err(error) => Err(error).with_context(|| format!("load env file {path}")),
    }
}

#[derive(Clone, Debug)]
pub struct Config {
    pub database_url: Option<String>,
    pub telegram_bot_token: String,
    pub telegram_required_group_id: String,
    pub template_catalog_url: String,
    pub template_preview_domain: String,
    pub template_preview_base_url: String,
    pub admin_contact: String,
    pub template_prices_usdt: Vec<Decimal>,
    pub ai_creation_price_usdt: Decimal,
    pub usdt_erc20_address: String,
    pub usdt_tron_address: String,
    pub usdt_sol_address: String,
    pub usdt_erc20_rpc_url: String,
    pub usdt_erc20_contract: String,
    pub usdt_erc20_min_confirmations: u64,
    pub tron_grid_base_url: String,
    pub tron_grid_api_key: Option<String>,
    pub usdt_tron_contract: String,
    pub usdt_tron_min_confirmations: u64,
    pub solana_rpc_url: String,
    pub usdt_sol_mint: String,
    pub usdt_sol_min_confirmations: u64,
    pub generation_max_concurrency: usize,
    pub generation_primary_provider: GenerationProviderName,
    pub agent_timeout_seconds: u64,
    pub gemini_agent_command: String,
    pub gemini_agent_args: String,
    pub codex_agent_command: String,
    pub codex_agent_args: String,
    pub claude_agent_command: String,
    pub claude_agent_args: String,
    pub generated_sites_dir: String,
    pub local_templates_dir: String,
    pub github_owner: String,
    pub github_token: String,
    pub api_service_base_url: Option<String>,
    pub api_service_api_key: Option<String>,
    pub auto_purchase_after_tx: bool,
    pub skip_payment_verification: bool,
    pub skip_domain_purchase: bool,
}

#[derive(Clone, Debug)]
pub struct NamecheapConfig {
    pub api_user: String,
    pub api_key: String,
    pub username: String,
    pub client_ip: String,
    pub sandbox: bool,
    pub contact: NamecheapContactConfig,
}

#[derive(Clone, Debug)]
pub struct NamecheapContactConfig {
    pub first_name: String,
    pub last_name: String,
    pub address1: String,
    pub city: String,
    pub state_province: String,
    pub postal_code: String,
    pub country: String,
    pub phone: String,
    pub email: String,
    pub organization: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GenerationProviderName {
    Gemini,
    Codex,
    Claude,
}

impl GenerationProviderName {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Gemini => "gemini",
            Self::Codex => "codex",
            Self::Claude => "claude",
        }
    }
}

impl Config {
    pub fn from_env() -> Result<Self> {
        Ok(Self {
            database_url: env::var("DATABASE_URL")
                .ok()
                .or_else(|| Some("sqlite://./db.sqlite3".to_string())),
            telegram_bot_token: env::var("TELEGRAM_BOT_TOKEN").unwrap_or_default(),
            telegram_required_group_id: env::var("TELEGRAM_REQUIRED_GROUP_ID")
                .unwrap_or_else(|_| "@MeMe_Web_Support".to_string()),
            template_catalog_url: env::var("TEMPLATE_CATALOG_URL")
                .unwrap_or_else(|_| "https://example.com/templates".to_string()),
            template_preview_domain: env::var("TEMPLATE_PREVIEW_DOMAIN")
                .unwrap_or_else(|_| "web".to_string()),
            template_preview_base_url: env::var("TEMPLATE_PREVIEW_BASE_URL")
                .unwrap_or_else(|_| "https://memewebs.xyz".to_string()),
            admin_contact: env::var("ADMIN_CONTACT").unwrap_or_else(|_| "@admin".to_string()),
            template_prices_usdt: parse_template_prices(
                &env::var("TEMPLATE_PRICES_USDT").unwrap_or_default(),
            )?,
            ai_creation_price_usdt: env::var("AI_CREATION_PRICE_USDT")
                .unwrap_or_else(|_| "50".to_string())
                .parse()
                .context("parse AI_CREATION_PRICE_USDT")?,
            usdt_erc20_address: env::var("USDT_ERC20_ADDRESS").unwrap_or_default(),
            usdt_tron_address: env::var("USDT_TRON_ADDRESS").unwrap_or_default(),
            usdt_sol_address: env::var("USDT_SOL_ADDRESS").unwrap_or_default(),
            usdt_erc20_rpc_url: env::var("USDT_ERC20_RPC_URL").unwrap_or_default(),
            usdt_erc20_contract: env::var("USDT_ERC20_CONTRACT")
                .unwrap_or_else(|_| "0xdAC17F958D2ee523a2206206994597C13D831ec7".to_string()),
            usdt_erc20_min_confirmations: env::var("USDT_ERC20_MIN_CONFIRMATIONS")
                .unwrap_or_else(|_| "12".to_string())
                .parse()
                .context("parse USDT_ERC20_MIN_CONFIRMATIONS")?,
            tron_grid_base_url: env::var("TRON_GRID_BASE_URL")
                .unwrap_or_else(|_| "https://api.trongrid.io".to_string()),
            tron_grid_api_key: env::var("TRON_GRID_API_KEY")
                .ok()
                .filter(|value| !value.trim().is_empty()),
            usdt_tron_contract: env::var("USDT_TRON_CONTRACT")
                .unwrap_or_else(|_| "TR7NHqjeKQxGTCi8q8ZY4pL8otSzgjLj6t".to_string()),
            usdt_tron_min_confirmations: env::var("USDT_TRON_MIN_CONFIRMATIONS")
                .unwrap_or_else(|_| "20".to_string())
                .parse()
                .context("parse USDT_TRON_MIN_CONFIRMATIONS")?,
            solana_rpc_url: env::var("SOLANA_RPC_URL")
                .unwrap_or_else(|_| "https://api.mainnet-beta.solana.com".to_string()),
            usdt_sol_mint: env::var("USDT_SOL_MINT")
                .unwrap_or_else(|_| "Es9vMFrzaCERmJfrF4H2FYD4KCoT1r9tnmTaGJQG7uC".to_string()),
            usdt_sol_min_confirmations: env::var("USDT_SOL_MIN_CONFIRMATIONS")
                .unwrap_or_else(|_| "1".to_string())
                .parse()
                .context("parse USDT_SOL_MIN_CONFIRMATIONS")?,
            generation_max_concurrency: env::var("GENERATION_MAX_CONCURRENCY")
                .unwrap_or_else(|_| "2".to_string())
                .parse()
                .context("parse GENERATION_MAX_CONCURRENCY")?,
            generation_primary_provider: parse_provider(
                &env::var("GENERATION_PRIMARY_PROVIDER").unwrap_or_else(|_| "gemini".to_string()),
            )?,
            agent_timeout_seconds: env::var("AGENT_TIMEOUT_SECONDS")
                .unwrap_or_else(|_| "900".to_string())
                .parse()
                .context("parse AGENT_TIMEOUT_SECONDS")?,
            gemini_agent_command: env::var("GEMINI_AGENT_COMMAND")
                .unwrap_or_else(|_| "gemini".to_string()),
            gemini_agent_args: env::var("GEMINI_AGENT_ARGS")
                .unwrap_or_else(|_| "--yolo --skip-trust -p {prompt}".to_string()),
            codex_agent_command: env::var("CODEX_AGENT_COMMAND")
                .unwrap_or_else(|_| "codex".to_string()),
            codex_agent_args: env::var("CODEX_AGENT_ARGS").unwrap_or_else(|_| {
                "exec --dangerously-bypass-approvals-and-sandbox --cd {output_dir} {prompt}"
                    .to_string()
            }),
            claude_agent_command: env::var("CLAUDE_AGENT_COMMAND")
                .unwrap_or_else(|_| "claude".to_string()),
            claude_agent_args: env::var("CLAUDE_AGENT_ARGS").unwrap_or_else(|_| {
                "-p {prompt} --output-format stream-json --verbose --allowedTools Read Edit Write Bash --dangerously-skip-permissions".to_string()
            }),
            generated_sites_dir: env::var("GENERATED_SITES_DIR")
                .unwrap_or_else(|_| "./generated-sites".to_string()),
            local_templates_dir: env::var("LOCAL_TEMPLATES_DIR")
                .unwrap_or_else(|_| "./templates".to_string()),
            github_owner: env::var("GITHUB_OWNER").unwrap_or_default(),
            github_token: env::var("GITHUB_TOKEN").unwrap_or_default(),
            api_service_base_url: env::var("API_SERVICE_BASE_URL")
                .ok()
                .filter(|v| !v.is_empty()),
            api_service_api_key: env::var("API_SERVICE_API_KEY")
                .ok()
                .filter(|v| !v.is_empty()),
            auto_purchase_after_tx: env_bool("AUTO_PURCHASE_AFTER_TX", false),
            skip_payment_verification: env_bool("SKIP_PAYMENT_VERIFICATION", false),
            skip_domain_purchase: env_bool("SKIP_DOMAIN_PURCHASE", false),
        })
    }
}

fn env_bool(name: &str, default: bool) -> bool {
    env::var(name)
        .map(|value| {
            matches!(
                value.to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(default)
}

fn parse_provider(value: &str) -> Result<GenerationProviderName> {
    match value.to_ascii_lowercase().as_str() {
        "gemini" => Ok(GenerationProviderName::Gemini),
        "codex" => Ok(GenerationProviderName::Codex),
        "claude" => Ok(GenerationProviderName::Claude),
        other => anyhow::bail!("unsupported generation provider: {other}"),
    }
}

const DEFAULT_TEMPLATE_PRICES: &[i64] = &[30, 30, 30, 40, 40, 40, 50, 50, 50, 60, 60, 60];

fn parse_template_prices(value: &str) -> Result<Vec<Decimal>> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Ok(DEFAULT_TEMPLATE_PRICES
            .iter()
            .map(|p| Decimal::from(*p))
            .collect());
    }
    trimmed
        .split(',')
        .enumerate()
        .map(|(i, part)| {
            let part = part.trim();
            if part.is_empty() {
                anyhow::bail!(
                    "TEMPLATE_PRICES_USDT position {} is empty, expected a number",
                    i + 1
                );
            }
            part.parse::<i64>()
                .with_context(|| {
                    format!("parse TEMPLATE_PRICES_USDT position {}: \"{part}\"", i + 1)
                })
                .map(Decimal::from)
        })
        .collect()
}
