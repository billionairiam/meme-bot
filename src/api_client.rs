use std::time::Duration;

use async_trait::async_trait;
use reqwest::Client;
use serde::Deserialize;
use tokio::time::sleep;
use tracing::{error, info};

use crate::deployment::cloudflare::{publish_dir_from_site_dir, ZONE_POLL_INTERVAL};
use crate::deployment::github::{ensure_github_repo, git_output, push_site_to_github};
use crate::deployment::{DeploymentError, DeploymentRequest, DeploymentResult, SiteDeployer};
use crate::domain::{
    ApiErrorResponse, CloudflareZoneResult, DomainError, DomainPurchaseRequest,
    DomainPurchaseResult, DomainQuote, DomainQuoteRequest, DomainRegistrar,
};

// Domain binding can take up to 20 min (zone activation polling), so allow 30 min total
const PAGES_DEPLOY_TIMEOUT: Duration = Duration::from_secs(1800);
const MAX_DEPLOY_RETRIES: u32 = 3;

// ---- Shared helpers ----

async fn map_http_error(response: reqwest::Response) -> DomainError {
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    match serde_json::from_str::<ApiErrorResponse>(&body) {
        Ok(api_err) => match api_err.error.as_str() {
            "unsupported_tld" => DomainError::UnsupportedTld,
            "unavailable" => DomainError::Unavailable,
            "already_registered_to_account" => DomainError::AlreadyRegisteredToAccount,
            "premium_rejected" => DomainError::PremiumRejected,
            "missing_config" => DomainError::MissingConfig(api_err.message),
            "namecheap_error" => DomainError::Namecheap(api_err.message),
            "cloudflare_error" => DomainError::Cloudflare(api_err.message),
            "upstream_http_error" | "upstream_parse_error" => DomainError::Http(api_err.message),
            _ => DomainError::Http(format!("{status}: {}", api_err.message)),
        },
        Err(_) => DomainError::Http(format!("{status}: {body}")),
    }
}

// ---- HttpClientDomainRegistrar ----

pub struct HttpClientDomainRegistrar {
    base_url: String,
    api_key: String,
    client: Client,
}

impl HttpClientDomainRegistrar {
    pub fn new(base_url: String, api_key: String) -> Self {
        Self {
            base_url,
            api_key,
            client: Client::new(),
        }
    }
}

#[async_trait]
impl DomainRegistrar for HttpClientDomainRegistrar {
    async fn quote(&self, request: &DomainQuoteRequest) -> Result<DomainQuote, DomainError> {
        let response = self
            .client
            .post(format!("{}/api/v1/domain/quote", self.base_url))
            .header("X-API-Key", &self.api_key)
            .json(request)
            .timeout(Duration::from_secs(30))
            .send()
            .await
            .map_err(|e| DomainError::Http(e.to_string()))?;

        if response.status().is_success() {
            response
                .json()
                .await
                .map_err(|e| DomainError::Parse(e.to_string()))
        } else {
            Err(map_http_error(response).await)
        }
    }

    async fn purchase(
        &self,
        request: &DomainPurchaseRequest,
    ) -> Result<DomainPurchaseResult, DomainError> {
        let response = self
            .client
            .post(format!("{}/api/v1/domain/purchase", self.base_url))
            .header("X-API-Key", &self.api_key)
            .json(request)
            .timeout(Duration::from_secs(30))
            .send()
            .await
            .map_err(|e| DomainError::Http(e.to_string()))?;

        if response.status().is_success() {
            response
                .json()
                .await
                .map_err(|e| DomainError::Parse(e.to_string()))
        } else {
            Err(map_http_error(response).await)
        }
    }
}

// ---- HttpClientPagesDeployer ----

#[derive(Debug, Deserialize)]
struct PagesDeployResponse {
    cloudflare_pages_url: String,
    #[serde(default)]
    custom_domain: Option<String>,
}

pub struct HttpClientPagesDeployer {
    base_url: String,
    api_key: String,
    client: Client,
}

impl HttpClientPagesDeployer {
    pub fn new(base_url: String, api_key: String) -> Self {
        Self {
            base_url,
            api_key,
            client: Client::new(),
        }
    }
}

#[async_trait]
impl SiteDeployer for HttpClientPagesDeployer {
    async fn deploy(
        &self,
        request: &DeploymentRequest,
    ) -> Result<DeploymentResult, DeploymentError> {
        // Step 1: Ensure GitHub repo exists (no retry needed — idempotent)
        ensure_github_repo(
            &self.client,
            &request.repo_owner,
            &request.repo_name,
            &request.github_token,
        )
        .await?;

        // Step 2: Push to GitHub with retries
        let mut last_push_err = None;
        for attempt in 1..=MAX_DEPLOY_RETRIES {
            match push_site_to_github(request).await {
                Ok(()) => {
                    last_push_err = None;
                    break;
                }
                Err(e) => {
                    error!(
                        repo_name = %request.repo_name,
                        attempt,
                        error = %e,
                        "git push attempt failed"
                    );
                    last_push_err = Some(e);
                    if attempt < MAX_DEPLOY_RETRIES {
                        sleep(Duration::from_secs(attempt as u64 * 5)).await;
                    }
                }
            }
        }
        if let Some(e) = last_push_err {
            return Err(e);
        }

        let commit_sha: String = git_output(&request.site_dir, &["rev-parse", "HEAD"]).await?;

        // Step 3: If custom domain is set, wait for zone to become active
        // (zone creation + nameserver setup happens at payment time)
        if let Some(domain) = &request.custom_domain {
            info!(domain = %domain, "waiting for zone to become active before deploy");
            self.wait_zone_active(domain).await?;
            info!(domain = %domain, "zone is active, proceeding to deploy");
        }

        // Step 4: Deploy to Cloudflare Pages with retries
        let mut last_cf_err = None;
        for attempt in 1..=MAX_DEPLOY_RETRIES {
            info!(
                repo_name = %request.repo_name,
                project_name = %request.cloudflare_project_name,
                attempt,
                "cloudflare pages deploy attempt"
            );
            match self.call_pages_deploy(request, commit_sha.trim()).await {
                Ok(result) => return Ok(result),
                Err(e) => {
                    error!(
                        repo_name = %request.repo_name,
                        attempt,
                        error = %e,
                        "cloudflare pages deploy attempt failed"
                    );
                    last_cf_err = Some(e);
                    if attempt < MAX_DEPLOY_RETRIES {
                        sleep(Duration::from_secs(attempt as u64 * 5)).await;
                    }
                }
            }
        }
        Err(last_cf_err.unwrap_or_else(|| {
            DeploymentError::CloudflarePages("deploy failed after retries".to_string())
        }))
    }
}

impl HttpClientPagesDeployer {
    /// Check zone status via the api-server.
    async fn get_zone_status(
        &self,
        domain: &str,
    ) -> Result<Option<(String, String)>, DeploymentError> {
        let response = self
            .client
            .post(format!("{}/api/v1/dns/zone-status", self.base_url))
            .header("X-API-Key", &self.api_key)
            .json(&serde_json::json!({"domain": domain}))
            .timeout(Duration::from_secs(30))
            .send()
            .await
            .map_err(|e| DeploymentError::DomainBind(format!("zone status request: {e}")))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(DeploymentError::DomainBind(format!(
                "zone status failed: {status}: {body}"
            )));
        }

        let json: serde_json::Value = response
            .json()
            .await
            .map_err(|e| DeploymentError::DomainBind(format!("parse zone status: {e}")))?;

        let found = json.get("found").and_then(|v| v.as_bool()).unwrap_or(false);
        if !found {
            return Ok(None);
        }
        let zone_id = json
            .get("zone_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let status = json
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();
        Ok(Some((zone_id, status)))
    }

    /// Check that the zone is active. Polls twice (30s interval = 1 min max).
    /// If not active after 1 min, fail — zone should have been set up at payment time.
    async fn wait_zone_active(&self, domain: &str) -> Result<(), DeploymentError> {
        const MAX_ATTEMPTS: usize = 2;
        for attempt in 0..MAX_ATTEMPTS {
            match self.get_zone_status(domain).await? {
                Some((_, ref status)) if status == "active" => return Ok(()),
                Some((_, ref status)) => {
                    info!(domain = %domain, status = %status, attempt, "zone not active yet");
                    if status == "moved" || status == "deactivated" || status == "banned" {
                        return Err(DeploymentError::DomainBind(format!(
                            "zone {domain} has terminal status: {status}"
                        )));
                    }
                }
                None => {
                    info!(domain = %domain, attempt, "zone not found");
                }
            }
            if attempt < MAX_ATTEMPTS - 1 {
                sleep(ZONE_POLL_INTERVAL).await;
            }
        }
        Err(DeploymentError::DomainBind(format!(
            "zone {domain} is not active — contact admin to check domain/nameserver setup"
        )))
    }

    /// Create a Cloudflare DNS zone via the api-server.
    pub async fn create_zone(&self, domain: &str) -> Result<CloudflareZoneResult, DeploymentError> {
        let response = self
            .client
            .post(format!("{}/api/v1/dns/create-zone", self.base_url))
            .header("X-API-Key", &self.api_key)
            .json(&serde_json::json!({"domain": domain}))
            .timeout(Duration::from_secs(30))
            .send()
            .await
            .map_err(|e| DeploymentError::DomainBind(format!("create zone request: {e}")))?;

        if response.status().is_success() {
            response
                .json()
                .await
                .map_err(|e| DeploymentError::DomainBind(format!("parse zone response: {e}")))
        } else {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            Err(DeploymentError::DomainBind(format!(
                "create zone failed: {status}: {body}"
            )))
        }
    }

    /// Set nameservers on the domain registrar via the api-server.
    pub async fn set_nameservers(
        &self,
        domain: &str,
        nameservers: &[String],
    ) -> Result<(), DeploymentError> {
        let response = self
            .client
            .post(format!("{}/api/v1/dns/set-nameservers", self.base_url))
            .header("X-API-Key", &self.api_key)
            .json(&serde_json::json!({"domain": domain, "nameservers": nameservers}))
            .timeout(Duration::from_secs(30))
            .send()
            .await
            .map_err(|e| DeploymentError::DomainBind(format!("set nameservers request: {e}")))?;

        if response.status().is_success() {
            Ok(())
        } else {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            Err(DeploymentError::DomainBind(format!(
                "set nameservers failed: {status}: {body}"
            )))
        }
    }
    async fn call_pages_deploy(
        &self,
        request: &DeploymentRequest,
        commit_sha: &str,
    ) -> Result<DeploymentResult, DeploymentError> {
        let response = self
            .client
            .post(format!("{}/api/v1/pages/deploy", self.base_url))
            .header("X-API-Key", &self.api_key)
            .json(&serde_json::json!({
                "project_name": request.cloudflare_project_name,
                "github_owner": request.repo_owner,
                "github_repo": request.repo_name,
                "production_branch": request.production_branch,
                "publish_dir": publish_dir_from_site_dir(&request.site_dir),
                "custom_domain": request.custom_domain,
                "expected_commit_sha": commit_sha,
            }))
            .timeout(PAGES_DEPLOY_TIMEOUT)
            .send()
            .await
            .map_err(|e| DeploymentError::CloudflarePages(e.to_string()))?;

        let status = response.status();
        let body = response
            .text()
            .await
            .map_err(|e| DeploymentError::CloudflarePages(e.to_string()))?;

        if status.is_success() {
            let deploy_response: PagesDeployResponse = serde_json::from_str(&body)
                .map_err(|e| DeploymentError::CloudflarePages(format!("parse response: {e}")))?;
            Ok(DeploymentResult {
                github_owner: request.repo_owner.clone(),
                github_repo_name: request.repo_name.clone(),
                github_repo_url: format!(
                    "https://github.com/{}/{}",
                    request.repo_owner, request.repo_name
                ),
                github_commit_sha: commit_sha.to_string(),
                cloudflare_project_name: request.cloudflare_project_name.clone(),
                cloudflare_pages_url: deploy_response.cloudflare_pages_url,
                custom_domain: deploy_response.custom_domain,
            })
        } else {
            let err_msg = serde_json::from_str::<serde_json::Value>(&body)
                .ok()
                .and_then(|v| v.get("message").and_then(|m| m.as_str()).map(String::from))
                .unwrap_or(body);
            Err(DeploymentError::CloudflarePages(err_msg))
        }
    }
}
