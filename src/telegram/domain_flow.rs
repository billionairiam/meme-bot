use rust_decimal::Decimal;
use sqlx::SqlitePool;
use tokio::time::{Duration as TokioDuration, sleep};
use tracing::{info, warn};
use uuid::Uuid;

use crate::{
    api_client::{HttpClientDnsProvisioner, HttpClientDomainRegistrar},
    db,
    domain::{DomainError, DomainPurchaseRequest, DomainQuoteRequest, DomainRegistrar},
};

use super::helpers::{domain_error_for_user, html_escape};
use super::types::{EXTERNAL_STEP_MAX_ATTEMPTS, TelegramUiConfig};

pub(crate) async fn quote_domain_with_retries(
    registrar: &dyn DomainRegistrar,
    domain: &str,
) -> Result<crate::domain::DomainQuote, DomainError> {
    let request = DomainQuoteRequest {
        domain: domain.to_string(),
    };
    let mut last_error = None;
    for attempt in 1..=EXTERNAL_STEP_MAX_ATTEMPTS {
        info!(%domain, attempt, "checking domain quote");
        match registrar.quote(&request).await {
            Ok(quote) => {
                info!(
                    %domain,
                    attempt,
                    available = quote.available,
                    premium = quote.is_premium,
                    price_usd = %quote.price_usd,
                    "domain quote check succeeded"
                );
                return Ok(quote);
            }
            Err(error)
                if retryable_domain_error(&error) && attempt < EXTERNAL_STEP_MAX_ATTEMPTS =>
            {
                warn!(%domain, attempt, error = %error, "domain quote failed; retrying");
                last_error = Some(error);
                sleep(TokioDuration::from_secs(attempt as u64 * 2)).await;
            }
            Err(error) => return Err(error),
        }
    }
    Err(last_error
        .unwrap_or_else(|| DomainError::Namecheap("domain quote failed after retries".to_string())))
}

pub(crate) async fn purchase_domain_with_retries(
    registrar: &dyn DomainRegistrar,
    domain: &str,
) -> Result<crate::domain::DomainPurchaseResult, DomainError> {
    let request = DomainPurchaseRequest {
        domain: domain.to_string(),
        years: 1,
        privacy_enabled: true,
    };
    let mut last_error = None;
    for attempt in 1..=EXTERNAL_STEP_MAX_ATTEMPTS {
        info!(%domain, attempt, "purchasing domain");
        match registrar.purchase(&request).await {
            Ok(result) => {
                info!(
                    %domain,
                    attempt,
                    namecheap_order_id = %result.namecheap_order_id,
                    charged_amount_usd = %result.charged_amount_usd,
                    "domain purchase call succeeded"
                );
                return Ok(result);
            }
            Err(error)
                if retryable_domain_error(&error) && attempt < EXTERNAL_STEP_MAX_ATTEMPTS =>
            {
                warn!(%domain, attempt, error = %error, "domain purchase failed; retrying");
                last_error = Some(error);
                sleep(TokioDuration::from_secs(attempt as u64 * 3)).await;
            }
            Err(error) => return Err(error),
        }
    }
    Err(last_error.unwrap_or_else(|| {
        DomainError::Namecheap("domain purchase failed after retries".to_string())
    }))
}

pub(crate) async fn check_alternative_domains(
    registrar: &dyn DomainRegistrar,
    base_name: &str,
    exclude_tld: &str,
) -> Vec<crate::domain::DomainQuote> {
    let mut alternatives = Vec::new();
    for tld in crate::domain::SUGGESTED_TLDS {
        if *tld == exclude_tld {
            continue;
        }
        let domain = format!("{base_name}.{tld}");
        let request = DomainQuoteRequest { domain };
        if let Ok(quote) = registrar.quote(&request).await
            && quote.available
            && !quote.is_premium
        {
            alternatives.push(quote);
        }
    }
    alternatives.sort_by_key(|quote| quote.price_usd);
    alternatives
}

pub(crate) async fn maybe_purchase_domain_after_tx(
    config: &TelegramUiConfig,
    pool: &SqlitePool,
    quote_id: Option<Uuid>,
    domain: Option<&str>,
) -> (bool, String) {
    if !config.namecheap.auto_purchase_after_tx {
        info!(
            quote_id = %quote_id.map(|id| id.to_string()).unwrap_or_else(|| "<none>".to_string()),
            domain = domain.unwrap_or("<none>"),
            "automatic domain purchase disabled"
        );
        return (
            false,
            "<b>Domain:</b> purchase disabled. Contact admin.".to_string(),
        );
    }

    let Some(domain) = domain else {
        warn!(
            quote_id = %quote_id.map(|id| id.to_string()).unwrap_or_else(|| "<none>".to_string()),
            "domain purchase skipped because no domain is attached"
        );
        return (false, "<b>Domain:</b> missing. Contact admin.".to_string());
    };
    let Some(quote_id) = quote_id else {
        warn!(
            domain = %domain,
            "domain purchase skipped because no persisted quote is attached"
        );
        return (
            false,
            "<b>Domain:</b> quote missing. Contact admin.".to_string(),
        );
    };
    info!(
        quote_id = %quote_id,
        domain = %domain,
        "starting post-payment domain purchase flow"
    );
    let (Some(base_url), Some(api_key)) = (
        config.api_service_base_url.as_deref(),
        config.api_service_api_key.as_deref(),
    ) else {
        warn!(
            quote_id = %quote_id,
            domain = %domain,
            "domain purchase skipped: API_SERVICE_BASE_URL/API_SERVICE_API_KEY not configured"
        );
        return (
            false,
            "<b>Domain:</b> API server not configured. Contact admin.".to_string(),
        );
    };
    let registrar: Box<dyn DomainRegistrar> = Box::new(HttpClientDomainRegistrar::new(
        base_url.to_string(),
        api_key.to_string(),
    ));

    let quote_record = match db::get_domain_quote_for_purchase(pool, quote_id).await {
        Ok(record) => record,
        Err(error) => {
            warn!(?error, quote_id = %quote_id, "failed to load persisted domain quote");
            let _ = db::mark_domain_purchase_manual_review(
                pool,
                quote_id,
                "persisted domain quote could not be loaded before purchase",
            )
            .await;
            return (false, "<b>Domain:</b> manual review required.".to_string());
        }
    };
    if !quote_record.domain.eq_ignore_ascii_case(domain) {
        warn!(
            quote_id = %quote_id,
            requested_domain = %domain,
            quoted_domain = %quote_record.domain,
            "domain quote mismatch before purchase"
        );
        let _ = db::mark_domain_purchase_manual_review(
            pool,
            quote_id,
            "persisted quote domain does not match requested domain",
        )
        .await;
        return (
            false,
            "<b>Domain:</b> quote mismatch. Contact admin.".to_string(),
        );
    }

    let price_tolerance = Decimal::new(1, 2);
    match quote_domain_with_retries(registrar.as_ref(), domain).await {
        Ok(current_quote) if current_quote.available && !current_quote.is_premium => {
            info!(
                quote_id = %quote_id,
                domain = %domain,
                current_price_usd = %current_quote.price_usd,
                paid_quote_usdt = %quote_record.quoted_amount_usdt,
                "domain price refreshed before purchase"
            );
            if current_quote.price_usd > quote_record.quoted_amount_usdt + price_tolerance {
                let reason = format!(
                    "current Namecheap price {} exceeds paid domain quote {}",
                    current_quote.price_usd, quote_record.quoted_amount_usdt
                );
                let _ = db::mark_domain_purchase_manual_review(pool, quote_id, &reason).await;
                return (
                    false,
                    format!(
                        "<b>Domain:</b> price changed (<code>{}</code> > <code>{}</code>). Contact admin.",
                        current_quote.price_usd, quote_record.quoted_amount_usdt
                    ),
                );
            }
        }
        Ok(_) => {
            let _ = db::mark_domain_purchase_manual_review(
                pool,
                quote_id,
                "domain became unavailable or premium before purchase",
            )
            .await;
            return (
                false,
                "<b>Domain:</b> unavailable. Contact admin.".to_string(),
            );
        }
        Err(error) => {
            let _ = db::mark_domain_purchase_manual_review(
                pool,
                quote_id,
                "domain price refresh failed before purchase",
            )
            .await;
            return (
                false,
                format!(
                    "<b>Domain:</b> price check failed. {}",
                    html_escape(&domain_error_for_user(&error))
                ),
            );
        }
    }

    if let Err(error) = db::record_domain_purchase_started(pool, quote_id).await {
        warn!(?error, quote_id = %quote_id, "failed to record domain purchase start");
    }
    info!(
        quote_id = %quote_id,
        domain = %domain,
        "submitting domain purchase"
    );
    match purchase_domain_with_retries(registrar.as_ref(), domain).await {
        Ok(result) => {
            info!(
                quote_id = %quote_id,
                domain = %domain,
                namecheap_order_id = %result.namecheap_order_id,
                charged_amount_usd = %result.charged_amount_usd,
                "domain purchase succeeded"
            );
            if let Err(error) = db::record_domain_purchase_success(pool, quote_id, &result).await {
                warn!(?error, quote_id = %quote_id, "failed to record domain purchase success");
            }
            if result.charged_amount_usd > quote_record.quoted_amount_usdt + price_tolerance {
                let reason = format!(
                    "Namecheap charged {} after paid domain quote {} (initial Namecheap quote {})",
                    result.charged_amount_usd,
                    quote_record.quoted_amount_usdt,
                    quote_record.namecheap_price_usd
                );
                let _ = db::mark_domain_purchase_manual_review(pool, quote_id, &reason).await;
                return (
                    false,
                    format!(
                        "<b>Domain bought.</b> Charge <code>{}</code> exceeds quote <code>{}</code>. Contact admin.",
                        result.charged_amount_usd, quote_record.quoted_amount_usdt
                    ),
                );
            }
            let dns_result = setup_cloudflare_dns_after_purchase_with_retries(config, domain).await;
            let dns_ready = match &dns_result {
                Ok(zone) => {
                    info!(
                        quote_id = %quote_id,
                        domain = %domain,
                        cloudflare_zone_id = %zone.zone_id,
                        nameservers = ?zone.nameservers,
                        "dns setup succeeded after domain purchase"
                    );
                    if let Err(error) = db::record_domain_dns_result(pool, quote_id, Ok(zone)).await
                    {
                        warn!(?error, quote_id = %quote_id, "failed to record dns success");
                    }
                    true
                }
                Err(message) => {
                    warn!(
                        quote_id = %quote_id,
                        domain = %domain,
                        error = %message,
                        "dns setup failed after domain purchase"
                    );
                    if let Err(error) =
                        db::record_domain_dns_result(pool, quote_id, Err(message.as_str())).await
                    {
                        warn!(?error, quote_id = %quote_id, "failed to record dns failure");
                    }
                    false
                }
            };
            let dns_message = match dns_result {
                Ok(zone) => format!(
                    "<b>DNS configured.</b> Zone <code>{}</code>.",
                    html_escape(&zone.zone_id)
                ),
                Err(message) => message,
            };
            (
                dns_ready,
                format!(
                    "<b>Domain purchased.</b>\nOrder: <code>{}</code>\nTX: <code>{}</code>\n{}",
                    html_escape(&result.namecheap_order_id),
                    html_escape(&result.namecheap_transaction_id),
                    dns_message
                ),
            )
        }
        Err(error) => {
            warn!(
                quote_id = %quote_id,
                domain = %domain,
                error = %error,
                "domain purchase failed"
            );
            if matches!(&error, DomainError::AlreadyRegisteredToAccount) {
                if let Err(db_error) = db::mark_domain_purchase_manual_review(
                    pool,
                    quote_id,
                    "domain appears registered in Namecheap account; reconcile order manually",
                )
                .await
                {
                    warn!(?db_error, quote_id = %quote_id, "failed to record domain purchase manual review");
                }
            } else if let Err(db_error) = db::mark_domain_purchase_failed(pool, quote_id).await {
                warn!(?db_error, quote_id = %quote_id, "failed to record domain purchase failure");
            }
            (
                false,
                format!(
                    "<b>Domain failed.</b>\n{}",
                    html_escape(&domain_error_for_user(&error))
                ),
            )
        }
    }
}

async fn setup_cloudflare_dns_after_purchase_with_retries(
    config: &TelegramUiConfig,
    domain: &str,
) -> Result<crate::domain::CloudflareZoneResult, String> {
    let mut last_error = None;
    for attempt in 1..=EXTERNAL_STEP_MAX_ATTEMPTS {
        info!(%domain, attempt, "setting up cloudflare/namecheap dns");
        match setup_cloudflare_dns_after_purchase(config, domain).await {
            Ok(zone) => {
                info!(
                    %domain,
                    attempt,
                    cloudflare_zone_id = %zone.zone_id,
                    nameservers = ?zone.nameservers,
                    "dns setup call succeeded"
                );
                return Ok(zone);
            }
            Err(error) if attempt < EXTERNAL_STEP_MAX_ATTEMPTS => {
                warn!(%domain, attempt, error = %error, "dns setup failed; retrying");
                last_error = Some(error);
                sleep(TokioDuration::from_secs(attempt as u64 * 3)).await;
            }
            Err(error) => {
                warn!(%domain, attempt, error = %error, "dns setup failed");
                return Err(error);
            }
        }
    }
    Err(last_error.unwrap_or_else(|| {
        "\n<b>DNS setup failed.</b> Repeated attempts failed. Contact admin.".to_string()
    }))
}

async fn setup_cloudflare_dns_after_purchase(
    config: &TelegramUiConfig,
    domain: &str,
) -> Result<crate::domain::CloudflareZoneResult, String> {
    let (Some(base_url), Some(api_key)) = (
        config.api_service_base_url.as_deref(),
        config.api_service_api_key.as_deref(),
    ) else {
        return Err(
            "\n<b>DNS setup failed.</b> API server not configured. Contact admin.".to_string(),
        );
    };

    let dns = HttpClientDnsProvisioner::new(base_url.to_string(), api_key.to_string());
    info!(%domain, "creating cloudflare zone through api-server");
    match dns.create_zone(domain).await {
        Ok(zone) => match dns
            .set_namecheap_nameservers(domain, &zone.nameservers)
            .await
        {
            Ok(()) => {
                info!(
                    %domain,
                    cloudflare_zone_id = %zone.zone_id,
                    nameservers = ?zone.nameservers,
                    "namecheap nameservers updated through api-server"
                );
                Ok(zone)
            }
            Err(error) => Err(format!(
                "\n<b>DNS nameserver update failed.</b> {}",
                html_escape(&domain_error_for_user(&error))
            )),
        },
        Err(error) => Err(format!(
            "\n<b>Cloudflare zone creation failed.</b> {}",
            html_escape(&domain_error_for_user(&error))
        )),
    }
}

fn retryable_domain_error(error: &DomainError) -> bool {
    matches!(
        error,
        DomainError::Http(_) | DomainError::Namecheap(_) | DomainError::Cloudflare(_)
    )
}
