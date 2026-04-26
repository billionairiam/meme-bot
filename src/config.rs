use std::env;

use anyhow::{Context, Result};

#[derive(Clone, Debug)]
pub struct Config {
    pub database_url: Option<String>,
    pub telegram_bot_token: String,
    pub telegram_required_group_id: String,
    pub telegram_frontend_only: bool,
    pub frontend_run_agent_after_tx: bool,
    pub frontend_deploy_after_agent: bool,
    pub template_catalog_url: String,
    pub admin_contact: String,
    pub usdt_erc20_address: String,
    pub usdt_tron_address: String,
    pub usdt_sol_address: String,
    pub generation_max_concurrency: usize,
    pub generation_primary_provider: GenerationProviderName,
    pub agent_timeout_seconds: u64,
    pub gemini_agent_command: String,
    pub gemini_agent_args: String,
    pub codex_agent_command: String,
    pub codex_agent_args: String,
    pub generated_sites_dir: String,
    pub local_templates_dir: String,
    pub github_owner: String,
    pub github_token: String,
    pub cloudflare_account_id: String,
    pub cloudflare_api_token: String,
    pub cloudflare_wrangler_command: String,
    pub cloudflare_wrangler_args: String,
    pub namecheap: NamecheapConfig,
}

#[derive(Clone, Debug)]
pub struct NamecheapConfig {
    pub api_user: String,
    pub api_key: String,
    pub username: String,
    pub client_ip: String,
    pub sandbox: bool,
    pub auto_purchase_after_tx: bool,
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
}

impl GenerationProviderName {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Gemini => "gemini",
            Self::Codex => "codex",
        }
    }
}

impl Config {
    pub fn from_env() -> Result<Self> {
        Ok(Self {
            database_url: env::var("DATABASE_URL").ok(),
            telegram_bot_token: env::var("TELEGRAM_BOT_TOKEN").unwrap_or_default(),
            telegram_required_group_id: env::var("TELEGRAM_REQUIRED_GROUP_ID")
                .unwrap_or_else(|_| "@MeMe_Web_Support".to_string()),
            telegram_frontend_only: env_bool("TELEGRAM_FRONTEND_ONLY", true),
            frontend_run_agent_after_tx: env_bool("FRONTEND_RUN_AGENT_AFTER_TX", false),
            frontend_deploy_after_agent: env_bool("FRONTEND_DEPLOY_AFTER_AGENT", false),
            template_catalog_url: env::var("TEMPLATE_CATALOG_URL")
                .unwrap_or_else(|_| "https://example.com/templates".to_string()),
            admin_contact: env::var("ADMIN_CONTACT").unwrap_or_else(|_| "@admin".to_string()),
            usdt_erc20_address: env::var("USDT_ERC20_ADDRESS").unwrap_or_default(),
            usdt_tron_address: env::var("USDT_TRON_ADDRESS").unwrap_or_default(),
            usdt_sol_address: env::var("USDT_SOL_ADDRESS").unwrap_or_default(),
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
                .unwrap_or_else(|_| "--yolo --prompt-file {prompt_file}".to_string()),
            codex_agent_command: env::var("CODEX_AGENT_COMMAND")
                .unwrap_or_else(|_| "codex".to_string()),
            codex_agent_args: env::var("CODEX_AGENT_ARGS").unwrap_or_else(|_| {
                "exec --dangerously-bypass-approvals-and-sandbox --cd {output_dir} --file {prompt_file}"
                    .to_string()
            }),
            generated_sites_dir: env::var("GENERATED_SITES_DIR")
                .unwrap_or_else(|_| "./generated-sites".to_string()),
            local_templates_dir: env::var("LOCAL_TEMPLATES_DIR")
                .unwrap_or_else(|_| "./templates".to_string()),
            github_owner: env::var("GITHUB_OWNER").unwrap_or_default(),
            github_token: env::var("GITHUB_TOKEN").unwrap_or_default(),
            cloudflare_account_id: env::var("CLOUDFLARE_ACCOUNT_ID").unwrap_or_default(),
            cloudflare_api_token: env::var("CLOUDFLARE_API_TOKEN").unwrap_or_default(),
            cloudflare_wrangler_command: env::var("CLOUDFLARE_WRANGLER_COMMAND")
                .unwrap_or_else(|_| "npx".to_string()),
            cloudflare_wrangler_args: env::var("CLOUDFLARE_WRANGLER_ARGS").unwrap_or_else(|_| {
                "wrangler pages deploy {site_dir} --project-name={project_name} --branch={branch}"
                    .to_string()
            }),
            namecheap: NamecheapConfig {
                api_user: env::var("NAMECHEAP_API_USER").unwrap_or_default(),
                api_key: env::var("NAMECHEAP_API_KEY").unwrap_or_default(),
                username: env::var("NAMECHEAP_USERNAME").unwrap_or_default(),
                client_ip: env::var("NAMECHEAP_CLIENT_IP").unwrap_or_default(),
                sandbox: env::var("NAMECHEAP_SANDBOX")
                    .unwrap_or_else(|_| "true".to_string())
                    .eq_ignore_ascii_case("true"),
                auto_purchase_after_tx: env_bool("NAMECHEAP_AUTO_PURCHASE_AFTER_TX", false),
                contact: NamecheapContactConfig {
                    first_name: env::var("NAMECHEAP_CONTACT_FIRST_NAME").unwrap_or_default(),
                    last_name: env::var("NAMECHEAP_CONTACT_LAST_NAME").unwrap_or_default(),
                    address1: env::var("NAMECHEAP_CONTACT_ADDRESS1").unwrap_or_default(),
                    city: env::var("NAMECHEAP_CONTACT_CITY").unwrap_or_default(),
                    state_province: env::var("NAMECHEAP_CONTACT_STATE").unwrap_or_default(),
                    postal_code: env::var("NAMECHEAP_CONTACT_POSTAL_CODE").unwrap_or_default(),
                    country: env::var("NAMECHEAP_CONTACT_COUNTRY").unwrap_or_default(),
                    phone: env::var("NAMECHEAP_CONTACT_PHONE").unwrap_or_default(),
                    email: env::var("NAMECHEAP_CONTACT_EMAIL").unwrap_or_default(),
                    organization: env::var("NAMECHEAP_CONTACT_ORGANIZATION")
                        .ok()
                        .filter(|value| !value.trim().is_empty()),
                },
            },
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
        other => anyhow::bail!("unsupported generation provider: {other}"),
    }
}
