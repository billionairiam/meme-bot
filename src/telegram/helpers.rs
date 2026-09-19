use std::path::Path;

use teloxide::prelude::*;
use teloxide::types::Message;
use tracing::info;

pub(crate) fn message_kind(msg: &Message) -> &'static str {
    if msg.text().is_some() {
        "text"
    } else if msg.photo().is_some() {
        "photo"
    } else if msg.document().is_some() {
        "document"
    } else {
        "other"
    }
}

pub(crate) fn normalize_domain(text: &str) -> String {
    text.trim()
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .trim_start_matches("www.")
        .trim_matches('/')
        .to_ascii_lowercase()
}

pub(crate) fn looks_like_tx_hash(text: &str) -> bool {
    let value = text.trim();
    value.len() >= 16 && value.chars().all(|ch| ch.is_ascii_alphanumeric())
}

pub(crate) fn configured_proxy() -> Option<String> {
    std::env::var("TELOXIDE_PROXY")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

pub(crate) fn html_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

pub(crate) fn payment_error_for_user(error: &crate::payment::PaymentVerificationError) -> String {
    use crate::payment::PaymentVerificationError;
    match error {
        PaymentVerificationError::NotFound => "TX not found.".to_string(),
        PaymentVerificationError::WrongChain => "Wrong chain.".to_string(),
        PaymentVerificationError::WrongToken => "Not a supported USDT transfer.".to_string(),
        PaymentVerificationError::WrongRecipient => "Wrong recipient.".to_string(),
        PaymentVerificationError::InsufficientAmount => "Amount too low.".to_string(),
        PaymentVerificationError::InsufficientConfirmations => {
            "Not enough confirmations.".to_string()
        }
        PaymentVerificationError::AlreadyUsed => "TX already used.".to_string(),
        PaymentVerificationError::ExpiredOrder => "Payment window expired.".to_string(),
        PaymentVerificationError::OldTransaction => {
            "TX is older than this payment request.".to_string()
        }
        PaymentVerificationError::TransactionAfterSubmission => {
            "TX is not confirmed yet. Try again later.".to_string()
        }
        PaymentVerificationError::Provider(value) => {
            format!("Payment provider error: {value}. Contact admin.")
        }
    }
}

#[allow(dead_code)]
pub(crate) fn domain_error_for_user(error: &crate::domain::DomainError) -> String {
    use crate::domain::DomainError;
    match error {
        DomainError::MissingConfig(value) => {
            format!("Namecheap configuration is missing: {value}")
        }
        DomainError::UnsupportedTld => {
            "Unsupported TLD. Supported: .com, .net, .org, .xyz, .fun, .site, .online, .io, .co, .me, .cc, .app, .dev, .world, .space, .club, .store, .shop, .vip".to_string()
        }
        DomainError::Unavailable => "Domain is unavailable.".to_string(),
        DomainError::AlreadyRegisteredToAccount => {
            "Domain appears registered in the Namecheap account. Contact admin for reconciliation."
                .to_string()
        }
        DomainError::PremiumRejected => "Premium/EAP domains require manual handling.".to_string(),
        DomainError::Namecheap(value) => format!("Namecheap returned an error: {value}"),
        DomainError::Http(value) => format!("Namecheap HTTP request failed: {value}"),
        DomainError::Parse(value) => format!("Could not parse Namecheap response: {value}"),
        DomainError::Cloudflare(value) => format!("Cloudflare DNS failed: {value}"),
    }
}

pub(crate) async fn download_telegram_logo(
    bot: &Bot,
    file_id: &str,
    order_id: &str,
) -> anyhow::Result<String> {
    let file = bot
        .get_file(teloxide::types::FileId::from(file_id.to_string()))
        .await?;
    let ext = Path::new(&file.path)
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_else(|| ".png".to_string());
    let dir = "./logos";
    tokio::fs::create_dir_all(dir).await?;
    let dest = format!("{dir}/{order_id}{ext}");
    let url = format!(
        "https://api.telegram.org/file/bot{}/{}",
        bot.token(),
        file.path
    );
    let response = reqwest::get(&url).await?;
    let bytes = response.bytes().await?;
    tokio::fs::write(&dest, &bytes).await?;
    info!(file_id = %file_id, dest = %dest, "downloaded logo from Telegram");
    Ok(dest)
}

pub(crate) fn is_message_not_modified(error: &teloxide::RequestError) -> bool {
    error
        .to_string()
        .to_ascii_lowercase()
        .contains("message is not modified")
}

/// Deserialize alternative domains JSON stored on an order.
pub(crate) fn parse_alternative_domains(
    json: Option<&str>,
) -> Option<Vec<crate::domain::DomainQuote>> {
    json.and_then(|s| serde_json::from_str(s).ok())
}
