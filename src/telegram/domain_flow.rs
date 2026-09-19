use tokio::time::{Duration as TokioDuration, sleep};
use tracing::{info, warn};

use crate::domain::{DomainError, DomainQuote, DomainQuoteRequest, DomainRegistrar};

use super::types::EXTERNAL_STEP_MAX_ATTEMPTS;

pub(crate) async fn quote_with_retries(
    registrar: &dyn DomainRegistrar,
    domain: &str,
) -> Result<DomainQuote, DomainError> {
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

pub(crate) async fn check_alternatives(
    registrar: &dyn DomainRegistrar,
    base_name: &str,
    exclude_tld: &str,
) -> Vec<DomainQuote> {
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

#[cfg(test)]
mod tests {
    use std::{
        collections::VecDeque,
        sync::{
            Mutex,
            atomic::{AtomicUsize, Ordering},
        },
    };

    use crate::domain::{DomainPurchaseRequest, DomainPurchaseResult, SUGGESTED_TLDS};

    use super::*;

    struct StubRegistrar {
        responses: Mutex<VecDeque<Result<DomainQuote, DomainError>>>,
        calls: AtomicUsize,
    }

    impl StubRegistrar {
        fn new(responses: Vec<Result<DomainQuote, DomainError>>) -> Self {
            Self {
                responses: Mutex::new(responses.into()),
                calls: AtomicUsize::new(0),
            }
        }

        fn call_count(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    #[async_trait::async_trait]
    impl DomainRegistrar for StubRegistrar {
        async fn quote(&self, _request: &DomainQuoteRequest) -> Result<DomainQuote, DomainError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.responses
                .lock()
                .unwrap()
                .pop_front()
                .expect("stub response for every quote")
        }

        async fn purchase(
            &self,
            _request: &DomainPurchaseRequest,
        ) -> Result<DomainPurchaseResult, DomainError> {
            unreachable!("purchase is not used by domain flow")
        }
    }

    fn quote(domain: &str, price: i64) -> DomainQuote {
        DomainQuote {
            domain: domain.to_string(),
            tld: domain
                .rsplit_once('.')
                .map(|(_, tld)| tld)
                .unwrap_or("")
                .to_string(),
            available: true,
            is_premium: false,
            price_usd: rust_decimal::Decimal::new(price, 0),
            years: 1,
        }
    }

    #[tokio::test(start_paused = true)]
    async fn retryable_quote_error_is_retried() {
        let registrar = StubRegistrar::new(vec![
            Err(DomainError::Http("temporary".to_string())),
            Ok(quote("coin.xyz", 12)),
        ]);
        let result = quote_with_retries(&registrar, "coin.xyz").await.unwrap();
        assert_eq!(result.domain, "coin.xyz");
        assert_eq!(registrar.call_count(), 2);
    }

    #[tokio::test]
    async fn non_retryable_quote_error_stops_immediately() {
        let registrar = StubRegistrar::new(vec![Err(DomainError::UnsupportedTld)]);
        let error = quote_with_retries(&registrar, "coin.invalid")
            .await
            .unwrap_err();
        assert!(matches!(error, DomainError::UnsupportedTld));
        assert_eq!(registrar.call_count(), 1);
    }

    #[tokio::test]
    async fn alternatives_exclude_requested_tld_filter_and_sort_by_price() {
        let responses = SUGGESTED_TLDS
            .iter()
            .filter(|tld| **tld != "com")
            .map(|tld| {
                let mut candidate = match *tld {
                    "xyz" => quote("coin.xyz", 20),
                    "fun" => quote("coin.fun", 5),
                    _ => quote(&format!("coin.{tld}"), 99),
                };
                if *tld == "site" {
                    candidate.is_premium = true;
                } else if !matches!(*tld, "xyz" | "fun") {
                    candidate.available = false;
                }
                Ok(candidate)
            })
            .collect();
        let registrar = StubRegistrar::new(responses);

        let alternatives = check_alternatives(&registrar, "coin", "com").await;

        assert_eq!(
            alternatives
                .iter()
                .map(|quote| quote.domain.as_str())
                .collect::<Vec<_>>(),
            ["coin.fun", "coin.xyz"]
        );
        assert_eq!(registrar.call_count(), SUGGESTED_TLDS.len() - 1);
    }
}
