use std::{path::Path, time::Duration};

use reqwest::Client;
use serde::Deserialize;
use tokio::time::sleep;
use tracing::{error, info, warn};

use crate::domain::{CloudflareZoneResult, DomainError};

use super::DeploymentError;

pub const PAGES_DEPLOYMENT_POLL_ATTEMPTS: usize = 36;
pub const PAGES_DEPLOYMENT_POLL_INTERVAL: Duration = Duration::from_secs(5);
pub const PAGES_DOMAIN_POLL_ATTEMPTS: usize = 120;
pub const PAGES_DOMAIN_POLL_INTERVAL: Duration = Duration::from_secs(10);
pub const ZONE_POLL_ATTEMPTS: usize = 40; // 40 * 30s = 20 min
pub const ZONE_POLL_INTERVAL: Duration = Duration::from_secs(30);

// ---- Request types ----

#[derive(Deserialize)]
pub struct PagesDeployRequest {
    pub project_name: String,
    pub github_owner: String,
    pub github_repo: String,
    pub production_branch: String,
    #[serde(default = "default_pages_publish_dir")]
    pub publish_dir: String,
    #[serde(default)]
    pub custom_domain: Option<String>,
    #[serde(default)]
    pub expected_commit_sha: Option<String>,
}

fn default_pages_publish_dir() -> String {
    "/".to_string()
}

// ---- Pages deployment helpers ----

#[derive(Debug)]
pub struct PagesDeploymentStatus {
    pub id: String,
    pub url: Option<String>,
    pub stage_status: Option<String>,
    pub commit_hash: Option<String>,
}

pub fn response_success(body: &str) -> Option<bool> {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|value| value.get("success").and_then(|success| success.as_bool()))
}

pub fn verify_existing_pages_project(
    body: &str,
    github_owner: &str,
    github_repo: &str,
    branch: &str,
    publish_dir: &str,
) -> Result<(), DeploymentError> {
    let value: serde_json::Value = serde_json::from_str(body)
        .map_err(|error| DeploymentError::CloudflarePages(format!("parse project: {error}")))?;
    if value.get("success").and_then(|success| success.as_bool()) != Some(true) {
        return Err(DeploymentError::CloudflarePages(format!(
            "get project returned unsuccessful response: {body}"
        )));
    }
    let result = value.get("result").ok_or_else(|| {
        DeploymentError::CloudflarePages("project response missing result".to_string())
    })?;
    let config = result
        .get("source")
        .and_then(|source| source.get("config"))
        .ok_or_else(|| {
            DeploymentError::CloudflarePages(
                "existing Pages project is not connected to a Git source".to_string(),
            )
        })?;
    let owner = config.get("owner").and_then(|item| item.as_str());
    let repo = config.get("repo_name").and_then(|item| item.as_str());
    let production_branch = config
        .get("production_branch")
        .and_then(|item| item.as_str());
    if owner != Some(github_owner) || repo != Some(github_repo) || production_branch != Some(branch)
    {
        return Err(DeploymentError::CloudflarePages(format!(
            "existing Pages project points to {}/{}:{}, expected {}/{}:{}",
            owner.unwrap_or("<missing>"),
            repo.unwrap_or("<missing>"),
            production_branch.unwrap_or("<missing>"),
            github_owner,
            github_repo,
            branch
        )));
    }

    if let Some(existing_dir) = result
        .get("build_config")
        .and_then(|config| config.get("destination_dir"))
        .and_then(|item| item.as_str())
        && !existing_dir.is_empty()
        && existing_dir != publish_dir
    {
        return Err(DeploymentError::CloudflarePages(format!(
            "existing Pages project publish directory is {existing_dir}, expected {publish_dir}"
        )));
    }

    Ok(())
}

fn parse_pages_deployment_response(
    status: reqwest::StatusCode,
    body: &str,
) -> Result<PagesDeploymentStatus, DeploymentError> {
    if !status.is_success() || response_success(body) == Some(false) {
        return Err(DeploymentError::CloudflarePages(format!(
            "{status}: {body}"
        )));
    }
    let value: serde_json::Value = serde_json::from_str(body).map_err(|e| {
        DeploymentError::CloudflarePages(format!("parse deployment response: {e}: {body}"))
    })?;
    let result = value.get("result").ok_or_else(|| {
        DeploymentError::CloudflarePages("deployment response missing result".to_string())
    })?;
    let id = result
        .get("id")
        .and_then(|item| item.as_str())
        .filter(|item| !item.trim().is_empty())
        .ok_or_else(|| {
            DeploymentError::CloudflarePages("deployment response missing id".to_string())
        })?
        .to_string();
    let url = result
        .get("url")
        .and_then(|item| item.as_str())
        .filter(|item| !item.trim().is_empty())
        .map(String::from);
    let stage_status = result
        .get("latest_stage")
        .and_then(|stage| stage.get("status"))
        .and_then(|item| item.as_str())
        .map(String::from);
    let commit_hash = extract_deployment_commit_hash(result);

    Ok(PagesDeploymentStatus {
        id,
        url,
        stage_status,
        commit_hash,
    })
}

pub fn extract_deployment_commit_hash(result: &serde_json::Value) -> Option<String> {
    [
        &["deployment_trigger", "metadata", "commit_hash"][..],
        &["deployment_trigger", "metadata", "commit_sha"][..],
        &["source", "config", "commit_hash"][..],
        &["source", "config", "commit_sha"][..],
    ]
    .iter()
    .find_map(|path| {
        let mut value = result;
        for key in *path {
            value = value.get(key)?;
        }
        value
            .as_str()
            .filter(|hash| !hash.trim().is_empty())
            .map(|hash| hash.trim().to_string())
    })
}

pub fn verify_deployment_commit(
    deployment: &PagesDeploymentStatus,
    expected_commit_sha: Option<&str>,
) -> Result<(), DeploymentError> {
    let Some(expected) = expected_commit_sha
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Ok(());
    };

    if let Some(actual) = deployment.commit_hash.as_deref()
        && !actual.eq_ignore_ascii_case(expected)
    {
        return Err(DeploymentError::CloudflarePages(format!(
            "deployment {} used commit {}, expected {}",
            deployment.id, actual, expected
        )));
    }

    Ok(())
}

pub fn ensure_domain_status_not_failed(domain: &str, status: &str) -> Result<(), DeploymentError> {
    match status {
        "active" | "pending" | "initializing" => {
            if status != "active" {
                info!(
                    "custom domain {domain} is {status}; DNS propagation may still be in progress"
                );
            }
            Ok(())
        }
        "blocked" | "error" | "deactivated" => Err(DeploymentError::DomainBind(format!(
            "custom domain {domain} has Cloudflare status {status}"
        ))),
        other => Err(DeploymentError::DomainBind(format!(
            "custom domain {domain} has unknown Cloudflare status {other}"
        ))),
    }
}

pub fn publish_dir_from_site_dir(site_dir: &str) -> &'static str {
    let root_index = Path::new(site_dir).join("index.html");
    if root_index.exists() { "/" } else { "dist" }
}

fn parse_existing_zone(body: &str) -> Option<CloudflareZoneResult> {
    let json: serde_json::Value = serde_json::from_str(body).ok()?;
    let zone = json.get("result")?.as_array()?.first()?;
    let zone_id = zone.get("id")?.as_str()?.to_string();
    let nameservers: Vec<String> = zone
        .get("name_servers")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|ns| ns.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    Some(CloudflareZoneResult {
        zone_id,
        nameservers,
    })
}

fn parse_zone_from_result(result: &serde_json::Value) -> Result<CloudflareZoneResult, DomainError> {
    let zone_id = result
        .get("id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| DomainError::Cloudflare("missing zone id".to_string()))?
        .to_string();
    let nameservers: Vec<String> = result
        .get("name_servers")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|ns| ns.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    Ok(CloudflareZoneResult {
        zone_id,
        nameservers,
    })
}

// ---- CloudflarePagesClient ----

pub struct CloudflarePagesClient {
    client: Client,
    account_id: String,
    api_token: String,
}

impl CloudflarePagesClient {
    pub fn new(account_id: String, api_token: String) -> Self {
        let client = Client::builder()
            .timeout(Duration::from_secs(60))
            .build()
            .expect("build reqwest client");
        Self {
            client,
            account_id,
            api_token,
        }
    }

    // ---- Zone operations ----

    /// Returns (zone_id, zone_status) if the zone exists.
    pub async fn get_zone_info(
        &self,
        domain: &str,
    ) -> Result<Option<(String, String)>, DeploymentError> {
        let response = self
            .client
            .get(format!(
                "https://api.cloudflare.com/client/v4/zones?name={domain}"
            ))
            .bearer_auth(&self.api_token)
            .send()
            .await
            .map_err(|e| DeploymentError::CloudflarePages(e.to_string()))?;
        if !response.status().is_success() {
            return Ok(None);
        }
        let body = response
            .text()
            .await
            .map_err(|e| DeploymentError::CloudflarePages(e.to_string()))?;
        let json: serde_json::Value = serde_json::from_str(&body)
            .map_err(|e| DeploymentError::CloudflarePages(format!("parse zone list: {e}")))?;
        let zone = match json
            .get("result")
            .and_then(|r| r.as_array())
            .and_then(|zones| zones.first())
        {
            Some(z) => z,
            None => return Ok(None),
        };
        let zone_id = zone
            .get("id")
            .and_then(|id| id.as_str())
            .unwrap_or("")
            .to_string();
        if zone_id.is_empty() {
            return Ok(None);
        }
        let zone_status = zone
            .get("status")
            .and_then(|s| s.as_str())
            .unwrap_or("unknown")
            .to_string();
        Ok(Some((zone_id, zone_status)))
    }

    /// Poll until the domain's zone status becomes "active", or timeout.
    /// Returns the zone_id on success.
    pub async fn wait_zone_active(&self, domain: &str) -> Result<String, DeploymentError> {
        for attempt in 0..ZONE_POLL_ATTEMPTS {
            match self.get_zone_info(domain).await? {
                Some((zone_id, ref status)) if status == "active" => return Ok(zone_id),
                Some((_, ref status)) => {
                    if attempt == 0 {
                        info!(domain = %domain, status = %status, "zone exists but not active yet, waiting");
                    }
                    ensure_domain_status_not_failed(domain, status)?;
                }
                None => {
                    if attempt == 0 {
                        info!(domain = %domain, "zone not found yet, waiting");
                    }
                }
            }
            sleep(ZONE_POLL_INTERVAL).await;
        }
        Err(DeploymentError::CloudflarePages(format!(
            "domain {domain} zone did not become active before timeout"
        )))
    }

    /// Create a Cloudflare DNS zone for the domain, or return the existing one.
    pub async fn create_zone(&self, domain: &str) -> Result<CloudflareZoneResult, DomainError> {
        // Check if zone already exists
        let existing = self
            .client
            .get(format!(
                "https://api.cloudflare.com/client/v4/zones?name={domain}"
            ))
            .bearer_auth(&self.api_token)
            .send()
            .await
            .map_err(|e| DomainError::Cloudflare(e.to_string()))?;
        if existing.status().is_success() {
            let body = existing
                .text()
                .await
                .map_err(|e| DomainError::Cloudflare(e.to_string()))?;
            if let Some(existing_zone) = parse_existing_zone(&body) {
                info!(domain = %domain, zone_id = %existing_zone.zone_id, "zone already exists");
                return Ok(existing_zone);
            }
        }

        let response = self
            .client
            .post("https://api.cloudflare.com/client/v4/zones")
            .bearer_auth(&self.api_token)
            .json(&serde_json::json!({
                "name": domain,
                "account": { "id": self.account_id },
                "jump_start": true,
            }))
            .send()
            .await
            .map_err(|e| {
                error!(domain = %domain, error = %e, "create zone http request failed");
                DomainError::Cloudflare(e.to_string())
            })?;

        let status = response.status();
        let body = response.text().await.map_err(|e| {
            error!(domain = %domain, error = %e, "create zone read response body failed");
            DomainError::Cloudflare(e.to_string())
        })?;

        if !status.is_success() {
            let err = DomainError::Cloudflare(format!("{status}: {body}"));
            error!(domain = %domain, error = %err, "create zone upstream error");
            return Err(err);
        }

        let json: serde_json::Value = serde_json::from_str(&body).map_err(|e| {
            error!(domain = %domain, error = %e, "create zone parse failed");
            DomainError::Cloudflare(format!("parse response: {e}"))
        })?;

        if json.get("success").and_then(|v| v.as_bool()) != Some(true) {
            let errors = json
                .get("errors")
                .map(|e| e.to_string())
                .unwrap_or_else(|| "unknown error".to_string());
            let err = DomainError::Cloudflare(format!("zone creation failed: {errors}"));
            error!(domain = %domain, error = %err, "create zone cloudflare rejected");
            return Err(err);
        }

        let result = json
            .get("result")
            .ok_or_else(|| DomainError::Cloudflare("missing result".to_string()))?;

        parse_zone_from_result(result)
    }

    // ---- Pages operations ----

    /// Trigger a new Pages deployment for the given project.
    pub async fn trigger_deployment(
        &self,
        project_name: &str,
    ) -> Result<PagesDeploymentStatus, DeploymentError> {
        let url = format!(
            "https://api.cloudflare.com/client/v4/accounts/{}/pages/projects/{}/deployments",
            self.account_id, project_name
        );
        let response = self
            .client
            .post(url)
            .bearer_auth(&self.api_token)
            .json(&serde_json::json!({}))
            .send()
            .await
            .map_err(|e| DeploymentError::CloudflarePages(e.to_string()))?;
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        parse_pages_deployment_response(status, &body)
    }

    /// Poll a deployment until it succeeds, fails, or times out.
    pub async fn wait_deployment_success(
        &self,
        project_name: &str,
        deployment_id: &str,
        expected_commit_sha: Option<&str>,
    ) -> Result<PagesDeploymentStatus, DeploymentError> {
        for _ in 0..PAGES_DEPLOYMENT_POLL_ATTEMPTS {
            let url = format!(
                "https://api.cloudflare.com/client/v4/accounts/{}/pages/projects/{}/deployments/{}",
                self.account_id, project_name, deployment_id
            );
            let response = self
                .client
                .get(url)
                .bearer_auth(&self.api_token)
                .send()
                .await
                .map_err(|e| DeploymentError::CloudflarePages(e.to_string()))?;
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            let deployment = parse_pages_deployment_response(status, &body)?;
            match deployment.stage_status.as_deref() {
                Some("success") => {
                    verify_deployment_commit(&deployment, expected_commit_sha)?;
                    return Ok(deployment);
                }
                Some("failure") | Some("canceled") => {
                    return Err(DeploymentError::CloudflarePages(format!(
                        "deployment {} ended with status {}",
                        deployment.id,
                        deployment.stage_status.unwrap_or_default()
                    )));
                }
                _ => sleep(PAGES_DEPLOYMENT_POLL_INTERVAL).await,
            }
        }

        Err(DeploymentError::CloudflarePages(format!(
            "deployment {deployment_id} did not complete before timeout"
        )))
    }

    /// Get the status of a custom domain bound to a Pages project.
    pub async fn get_pages_domain_status(
        &self,
        project_name: &str,
        domain: &str,
    ) -> Result<Option<String>, DeploymentError> {
        let url = format!(
            "https://api.cloudflare.com/client/v4/accounts/{}/pages/projects/{}/domains/{}",
            self.account_id, project_name, domain
        );
        let response = self
            .client
            .get(url)
            .bearer_auth(&self.api_token)
            .send()
            .await
            .map_err(|e| DeploymentError::CloudflarePages(e.to_string()))?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        let status = response.status();
        let body = response
            .text()
            .await
            .map_err(|e| DeploymentError::CloudflarePages(e.to_string()))?;
        if !status.is_success()
            || response_success(&body)
                .map(|success| !success)
                .unwrap_or(false)
        {
            return Err(DeploymentError::CloudflarePages(format!(
                "{status}: {body}"
            )));
        }
        let value: serde_json::Value = serde_json::from_str(&body).map_err(|e| {
            DeploymentError::CloudflarePages(format!("parse domain response: {e}: {body}"))
        })?;
        let domain_status = value
            .get("result")
            .and_then(|result| result.get("status"))
            .and_then(|s| s.as_str())
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| {
                DeploymentError::CloudflarePages("domain response missing status".to_string())
            })?;
        Ok(Some(domain_status.to_string()))
    }

    /// Create a CNAME DNS record pointing the custom domain to the Pages
    /// project's `*.pages.dev` subdomain. Idempotent: skips if a matching
    /// record already exists.
    async fn ensure_pages_cname(
        &self,
        zone_id: &str,
        domain: &str,
        project_name: &str,
    ) -> Result<(), DeploymentError> {
        let pages_target = format!("{project_name}.pages.dev");

        // Check if record already exists
        let list_url = format!(
            "https://api.cloudflare.com/client/v4/zones/{zone_id}/dns_records?type=CNAME&name={domain}"
        );
        let list_resp = self
            .client
            .get(&list_url)
            .bearer_auth(&self.api_token)
            .send()
            .await
            .map_err(|e| DeploymentError::DomainBind(format!("list dns records: {e}")))?;

        if list_resp.status().is_success() {
            let body: serde_json::Value = list_resp
                .json()
                .await
                .map_err(|e| DeploymentError::DomainBind(format!("parse dns list: {e}")))?;
            let records = body.get("result").and_then(|r| r.as_array());
            if let Some(records) = records {
                if records.iter().any(|r| {
                    r.get("content")
                        .and_then(|c| c.as_str())
                        .is_some_and(|c| c == pages_target)
                }) {
                    info!(domain = %domain, target = %pages_target, "CNAME record already exists");
                    return Ok(());
                }
            }
        }

        // Create the CNAME record
        let create_url = format!(
            "https://api.cloudflare.com/client/v4/zones/{zone_id}/dns_records"
        );
        let resp = self
            .client
            .post(&create_url)
            .bearer_auth(&self.api_token)
            .json(&serde_json::json!({
                "type": "CNAME",
                "name": domain,
                "content": pages_target,
                "proxied": true,
                "ttl": 1
            }))
            .send()
            .await
            .map_err(|e| DeploymentError::DomainBind(format!("create CNAME record: {e}")))?;

        let status = resp.status();
        let body = resp
            .text()
            .await
            .map_err(|e| DeploymentError::DomainBind(format!("read CNAME response: {e}")))?;

        if status.is_success() {
            info!(domain = %domain, target = %pages_target, "CNAME DNS record created");
            Ok(())
        } else {
            // 81057 = "Record already exists" — treat as success
            if body.contains("81057") {
                info!(domain = %domain, "CNAME record already exists (code 81057)");
                return Ok(());
            }
            Err(DeploymentError::DomainBind(format!(
                "create CNAME record failed: {status}: {body}"
            )))
        }
    }

    /// Bind a custom domain to a Pages project: create CNAME record, register
    /// the domain with Pages, and poll until active.
    pub async fn bind_custom_domain(
        &self,
        project_name: &str,
        domain: &str,
    ) -> Result<(), DeploymentError> {
        // Step 1: Get zone_id (zone should already be active — meme-bot waits for this)
        let zone_id = match self.get_zone_info(domain).await? {
            Some((zone_id, _)) => zone_id,
            None => {
                return Err(DeploymentError::DomainBind(format!(
                    "zone for {domain} not found in Cloudflare"
                )));
            }
        };

        // Step 2: Ensure CNAME record exists (domain → project.pages.dev)
        self.ensure_pages_cname(&zone_id, domain, project_name)
            .await?;

        // Step 3: If Pages domain is already bound, verify status.
        if let Some(pages_status) = self.get_pages_domain_status(project_name, domain).await? {
            ensure_domain_status_not_failed(domain, &pages_status)?;
            if pages_status == "active" {
                return Ok(());
            }
        } else {
            // Bind custom domain to Pages project.
            let bind_url = format!(
                "https://api.cloudflare.com/client/v4/accounts/{}/pages/projects/{}/domains",
                self.account_id, project_name
            );
            let bind_response = self
                .client
                .post(&bind_url)
                .bearer_auth(&self.api_token)
                .json(&serde_json::json!({"name": domain}))
                .send()
                .await
                .map_err(|e| DeploymentError::CloudflarePages(e.to_string()))?;

            let bind_status = bind_response.status();
            let bind_body = bind_response
                .text()
                .await
                .map_err(|e| DeploymentError::CloudflarePages(e.to_string()))?;

            if !bind_status.is_success()
                || response_success(&bind_body)
                    .map(|success| !success)
                    .unwrap_or(false)
            {
                warn!(?bind_status, %bind_body, "custom domain bind failed");
                return Err(DeploymentError::DomainBind(format!(
                    "custom domain bind failed: {bind_status}: {bind_body}"
                )));
            }
        }

        // Step 4: Poll until Pages domain status becomes active.
        for _ in 0..PAGES_DOMAIN_POLL_ATTEMPTS {
            let pages = self.get_pages_domain_status(project_name, domain).await?;
            match pages.as_deref() {
                Some("active") => return Ok(()),
                Some(pages_status) => {
                    ensure_domain_status_not_failed(domain, pages_status)?;
                    info!(
                        domain = %domain,
                        pages_status = %pages_status,
                        "custom domain not active yet, waiting"
                    );
                }
                _ => {}
            }
            sleep(PAGES_DOMAIN_POLL_INTERVAL).await;
        }

        Err(DeploymentError::DomainBind(format!(
            "custom domain {domain} did not become active before timeout"
        )))
    }

    /// Full Pages deployment: ensure/create project, trigger deployment,
    /// wait for success, and optionally bind a custom domain.
    pub async fn deploy(
        &self,
        request: &PagesDeployRequest,
    ) -> Result<serde_json::Value, DeploymentError> {
        let cf_url = format!(
            "https://api.cloudflare.com/client/v4/accounts/{}/pages/projects",
            self.account_id
        );

        let project_url = format!("{cf_url}/{}", request.project_name);
        let existing = self
            .client
            .get(&project_url)
            .bearer_auth(&self.api_token)
            .send()
            .await
            .map_err(|e| DeploymentError::CloudflarePages(e.to_string()))?;
        let project_exists = existing.status().is_success();
        if project_exists {
            let body = existing
                .text()
                .await
                .map_err(|e| DeploymentError::CloudflarePages(e.to_string()))?;
            verify_existing_pages_project(
                &body,
                &request.github_owner,
                &request.github_repo,
                &request.production_branch,
                &request.publish_dir,
            )?;
        } else if existing.status() != reqwest::StatusCode::NOT_FOUND {
            let status = existing.status();
            let body = existing
                .text()
                .await
                .map_err(|e| DeploymentError::CloudflarePages(e.to_string()))?;
            error!(project = %request.project_name, status = %status, body = %body, "get existing pages project failed");
            return Err(DeploymentError::CloudflarePages(format!(
                "get project failed: {status}: {body}"
            )));
        }

        if !project_exists {
            let response = self
                .client
                .post(&cf_url)
                .bearer_auth(&self.api_token)
                .json(&serde_json::json!({
                    "name": request.project_name,
                    "production_branch": request.production_branch,
                    "source": {
                        "type": "github",
                        "config": {
                            "owner": request.github_owner,
                            "repo_name": request.github_repo,
                            "production_branch": request.production_branch,
                            "production_deployments_enabled": true,
                            "preview_deployment_setting": "none",
                            "pr_comments_enabled": false,
                            "deployments_enabled": true,
                        }
                    },
                    "build_config": {
                        "build_command": "",
                        "destination_dir": request.publish_dir,
                        "root_dir": ""
                    }
                }))
                .send()
                .await
                .map_err(|e| DeploymentError::CloudflarePages(e.to_string()))?;

            let status = response.status();
            let body = response
                .text()
                .await
                .map_err(|e| DeploymentError::CloudflarePages(e.to_string()))?;

            if !status.is_success() {
                error!(status = %status, body = %body, "cloudflare pages project creation failed");
                return Err(DeploymentError::CloudflarePages(format!(
                    "create pages project failed: {status}: {body}"
                )));
            }

            let json: serde_json::Value = serde_json::from_str(&body)
                .map_err(|e| DeploymentError::CloudflarePages(format!("parse response: {e}")))?;

            if json.get("success").and_then(|v| v.as_bool()) != Some(true) {
                let errors = json
                    .get("errors")
                    .map(|e| e.to_string())
                    .unwrap_or_else(|| "unknown error".to_string());
                error!(project = %request.project_name, errors = %errors, "cloudflare pages project creation rejected");
                return Err(DeploymentError::CloudflarePages(format!(
                    "pages project creation failed: {errors}"
                )));
            }
        }

        let deployment = self.trigger_deployment(&request.project_name).await?;
        let deployment = self
            .wait_deployment_success(
                &request.project_name,
                &deployment.id,
                request.expected_commit_sha.as_deref(),
            )
            .await?;

        if let Some(domain) = &request.custom_domain {
            info!(domain = %domain, project = %request.project_name, "binding custom domain to pages project");
            match self.bind_custom_domain(&request.project_name, domain).await {
                Ok(()) => {
                    info!(domain = %domain, project = %request.project_name, "custom domain bound successfully");
                }
                Err(e) => {
                    error!(domain = %domain, project = %request.project_name, error = %e, "custom domain binding failed");
                    return Err(e);
                }
            }
        }

        let pages_url = deployment
            .url
            .unwrap_or_else(|| format!("https://{}.pages.dev", request.project_name));
        info!(url = %pages_url, "pages project created successfully");

        Ok(serde_json::json!({
            "cloudflare_pages_url": pages_url,
            "custom_domain": request.custom_domain,
        }))
    }
}
