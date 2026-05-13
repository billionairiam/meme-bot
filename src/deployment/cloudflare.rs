use std::{path::Path, time::Duration};

use reqwest::Client;
use tokio::time::sleep;

use crate::domain::CloudflareZoneResult;

use super::DeploymentError;

pub const PAGES_DEPLOYMENT_POLL_ATTEMPTS: usize = 36;
pub const PAGES_DEPLOYMENT_POLL_INTERVAL: Duration = Duration::from_secs(5);
pub const PAGES_DOMAIN_POLL_ATTEMPTS: usize = 120;
pub const PAGES_DOMAIN_POLL_INTERVAL: Duration = Duration::from_secs(10);
pub const ZONE_POLL_ATTEMPTS: usize = 40; // 40 * 30s = 20 min
pub const ZONE_POLL_INTERVAL: Duration = Duration::from_secs(30);

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

pub async fn parse_pages_deployment_response(
    response: reqwest::Response,
) -> Result<PagesDeploymentStatus, DeploymentError> {
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    if !status.is_success() || response_success(&body) == Some(false) {
        return Err(DeploymentError::CloudflarePages(format!(
            "{status}: {body}"
        )));
    }
    let value: serde_json::Value = serde_json::from_str(&body).map_err(|error| {
        DeploymentError::CloudflarePages(format!("parse deployment response: {error}: {body}"))
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
                tracing::info!(
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

// ---- Zone helpers (used by api-server) ----

pub fn parse_existing_zone(body: &str) -> Option<CloudflareZoneResult> {
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

/// Returns (zone_id, zone_status) if the zone exists.
pub async fn get_zone_info(
    client: &Client,
    api_token: &str,
    domain: &str,
) -> Result<Option<(String, String)>, DeploymentError> {
    let response = client
        .get(format!(
            "https://api.cloudflare.com/client/v4/zones?name={domain}"
        ))
        .bearer_auth(api_token)
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

/// Poll Cloudflare until the domain zone is visible, up to ~20 min.
pub async fn ensure_zone_exists(
    client: &Client,
    api_token: &str,
    domain: &str,
) -> Result<String, DeploymentError> {
    for attempt in 0..ZONE_POLL_ATTEMPTS {
        if let Some((zone_id, _status)) = get_zone_info(client, api_token, domain).await? {
            return Ok(zone_id);
        }
        if attempt == 0 {
            tracing::info!(domain = %domain, "zone not found yet, waiting for Cloudflare zone provisioning (up to 20 min)");
        }
        sleep(ZONE_POLL_INTERVAL).await;
    }
    Err(DeploymentError::CloudflarePages(format!(
        "domain {domain} zone still not visible in Cloudflare after waiting for zone provisioning"
    )))
}
