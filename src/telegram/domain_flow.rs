use tokio::time::{Duration as TokioDuration, sleep};
use tracing::{info, warn};

use crate::domain::{DomainError, DomainQuoteRequest, DomainRegistrar};

use super::types::EXTERNAL_STEP_MAX_ATTEMPTS;

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

fn retryable_domain_error(error: &DomainError) -> bool {
    matches!(
        error,
        DomainError::Http(_) | DomainError::Namecheap(_) | DomainError::Cloudflare(_)
    )
}
