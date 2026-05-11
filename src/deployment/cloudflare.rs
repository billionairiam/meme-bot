use std::{path::Path, process::Stdio, time::Duration};

use async_trait::async_trait;
use reqwest::{Client, StatusCode};
use serde::Deserialize;
use serde_json::json;
use tokio::process::Command;
use tokio::time::sleep;

use super::github::{ensure_github_repo, git_output, push_site_to_github};
use super::{DeploymentError, DeploymentRequest, DeploymentResult, SiteDeployer};

const PAGES_DEPLOYMENT_POLL_ATTEMPTS: usize = 36;
const PAGES_DEPLOYMENT_POLL_INTERVAL: Duration = Duration::from_secs(5);
const PAGES_DOMAIN_POLL_ATTEMPTS: usize = 12;
const PAGES_DOMAIN_POLL_INTERVAL: Duration = Duration::from_secs(5);
const API_PAGES_DEPLOY_TIMEOUT: Duration = Duration::from_secs(300);

// ---- Pages deployment helpers ----

#[derive(Debug)]
pub(crate) struct PagesDeploymentStatus {
    pub(crate) id: String,
    pub(crate) url: Option<String>,
    pub(crate) stage_status: Option<String>,
    pub(crate) commit_hash: Option<String>,
}

async fn ensure_cloudflare_success<F>(
    response: reqwest::Response,
    error_factory: F,
) -> Result<(), DeploymentError>
where
    F: Fn(String) -> DeploymentError,
{
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    if status.is_success() && response_success(&body).unwrap_or(true) {
        Ok(())
    } else {
        Err(error_factory(format!("{status}: {body}")))
    }
}

pub(crate) fn response_success(body: &str) -> Option<bool> {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|value| value.get("success").and_then(|success| success.as_bool()))
}

pub(crate) fn verify_existing_pages_project(
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
    if owner != Some(github_owner)
        || repo != Some(github_repo)
        || production_branch != Some(branch)
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

async fn parse_pages_deployment_response(
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

pub(crate) fn extract_deployment_commit_hash(result: &serde_json::Value) -> Option<String> {
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

pub(crate) fn verify_deployment_commit(
    deployment: &PagesDeploymentStatus,
    expected_commit_sha: Option<&str>,
) -> Result<(), DeploymentError> {
    let Some(expected) = expected_commit_sha
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Ok(());
    };
    match deployment.commit_hash.as_deref() {
        Some(actual) if actual.eq_ignore_ascii_case(expected) => Ok(()),
        Some(actual) => Err(DeploymentError::CloudflarePages(format!(
            "deployment {} used commit {}, expected {}",
            deployment.id, actual, expected
        ))),
        None => Err(DeploymentError::CloudflarePages(format!(
            "deployment {} did not report a commit hash; expected {}",
            deployment.id, expected
        ))),
    }
}

async fn parse_pages_domain_status_response(
    response: reqwest::Response,
) -> Result<String, DeploymentError> {
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    if !status.is_success() || response_success(&body) == Some(false) {
        return Err(DeploymentError::DomainBind(format!("{status}: {body}")));
    }
    let value: serde_json::Value = serde_json::from_str(&body).map_err(|error| {
        DeploymentError::DomainBind(format!("parse domain response: {error}: {body}"))
    })?;
    value
        .get("result")
        .and_then(|result| result.get("status"))
        .and_then(|status| status.as_str())
        .filter(|status| !status.trim().is_empty())
        .map(String::from)
        .ok_or_else(|| DeploymentError::DomainBind("domain response missing status".to_string()))
}

pub(crate) fn ensure_domain_status_not_failed(domain: &str, status: &str) -> Result<(), DeploymentError> {
    match status {
        "active" => Ok(()),
        "pending" | "initializing" => Err(DeploymentError::DomainBind(format!(
            "custom domain {domain} is not active yet; Cloudflare status {status}"
        ))),
        "blocked" | "error" | "deactivated" => Err(DeploymentError::DomainBind(format!(
            "custom domain {domain} has Cloudflare status {status}"
        ))),
        other => Err(DeploymentError::DomainBind(format!(
            "custom domain {domain} has unknown Cloudflare status {other}"
        ))),
    }
}

pub(crate) fn publish_dir_from_site_dir(site_dir: &str) -> &'static str {
    let root_index = Path::new(site_dir).join("index.html");
    if root_index.exists() { "/" } else { "dist" }
}

pub(crate) fn render_wrangler_args(
    args_template: &str,
    site_dir: &str,
    project_name: &str,
    branch: &str,
) -> Result<Vec<String>, DeploymentError> {
    let rendered = args_template
        .replace("{site_dir}", site_dir)
        .replace("{project_name}", project_name)
        .replace("{branch}", branch);

    shell_words::split(&rendered)
        .map_err(|error| DeploymentError::CloudflarePages(format!("invalid wrangler args: {error}")))
}

// ---- CloudflarePagesDeployer ----

pub struct CloudflarePagesDeployer {
    account_id: String,
    api_token: String,
    wrangler_command: String,
    wrangler_args: String,
    client: Client,
}

impl CloudflarePagesDeployer {
    pub fn new(
        account_id: String,
        api_token: String,
        wrangler_command: String,
        wrangler_args: String,
    ) -> Self {
        Self {
            account_id,
            api_token,
            wrangler_command,
            wrangler_args,
            client: Client::new(),
        }
    }

    fn validate_config(&self) -> Result<(), DeploymentError> {
        let missing = [
            ("CLOUDFLARE_ACCOUNT_ID", &self.account_id),
            ("CLOUDFLARE_API_TOKEN", &self.api_token),
            ("CLOUDFLARE_WRANGLER_COMMAND", &self.wrangler_command),
            ("CLOUDFLARE_WRANGLER_ARGS", &self.wrangler_args),
        ]
        .iter()
        .filter_map(|(name, value)| value.trim().is_empty().then_some(*name))
        .collect::<Vec<_>>();

        if missing.is_empty() {
            Ok(())
        } else {
            Err(DeploymentError::MissingConfig(missing.join(", ")))
        }
    }

    fn validate_cloudflare_config(&self) -> Result<(), DeploymentError> {
        let missing = [
            ("CLOUDFLARE_ACCOUNT_ID", &self.account_id),
            ("CLOUDFLARE_API_TOKEN", &self.api_token),
        ]
        .iter()
        .filter_map(|(name, value)| value.trim().is_empty().then_some(*name))
        .collect::<Vec<_>>();

        if missing.is_empty() {
            Ok(())
        } else {
            Err(DeploymentError::MissingConfig(missing.join(", ")))
        }
    }

    fn project_url(&self, project_name: &str) -> String {
        format!(
            "https://api.cloudflare.com/client/v4/accounts/{}/pages/projects/{}",
            self.account_id, project_name
        )
    }

    async fn ensure_project(
        &self,
        project_name: &str,
        branch: &str,
    ) -> Result<(), DeploymentError> {
        let url = self.project_url(project_name);
        let response = self
            .client
            .get(&url)
            .bearer_auth(&self.api_token)
            .send()
            .await
            .map_err(|error| DeploymentError::CloudflarePages(error.to_string()))?;

        if response.status().is_success() {
            return Ok(());
        }
        if response.status() != StatusCode::NOT_FOUND {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(DeploymentError::CloudflarePages(format!(
                "get project failed: {status}: {body}"
            )));
        }

        let create_url = format!(
            "https://api.cloudflare.com/client/v4/accounts/{}/pages/projects",
            self.account_id
        );
        let response = self
            .client
            .post(create_url)
            .bearer_auth(&self.api_token)
            .json(&json!({
                "name": project_name,
                "production_branch": branch
            }))
            .send()
            .await
            .map_err(|error| DeploymentError::CloudflarePages(error.to_string()))?;

        ensure_cloudflare_success(response, DeploymentError::CloudflarePages).await
    }

    async fn ensure_git_project(
        &self,
        project_name: &str,
        branch: &str,
        github_owner: &str,
        github_repo: &str,
        publish_dir: &str,
    ) -> Result<(), DeploymentError> {
        let get_response = self
            .client
            .get(self.project_url(project_name))
            .bearer_auth(&self.api_token)
            .send()
            .await
            .map_err(|error| DeploymentError::CloudflarePages(error.to_string()))?;
        if get_response.status().is_success() {
            let body = get_response.text().await.unwrap_or_default();
            return verify_existing_pages_project(
                &body, github_owner, github_repo, branch, publish_dir,
            );
        }
        if get_response.status() != StatusCode::NOT_FOUND {
            let status = get_response.status();
            let body = get_response.text().await.unwrap_or_default();
            return Err(DeploymentError::CloudflarePages(format!(
                "get project failed: {status}: {body}"
            )));
        }

        let url = format!(
            "https://api.cloudflare.com/client/v4/accounts/{}/pages/projects",
            self.account_id
        );
        let response = self
            .client
            .post(url)
            .bearer_auth(&self.api_token)
            .json(&json!({
                "name": project_name,
                "production_branch": branch,
                "build_config": {
                    "build_command": "",
                    "destination_dir": publish_dir,
                    "root_dir": ""
                },
                "source": {
                    "type": "github",
                    "config": {
                        "owner": github_owner,
                        "repo_name": github_repo,
                        "production_branch": branch,
                        "production_deployments_enabled": true,
                        "preview_deployment_setting": "none",
                        "pr_comments_enabled": false,
                        "deployments_enabled": true
                    }
                }
            }))
            .send()
            .await
            .map_err(|error| DeploymentError::CloudflarePages(error.to_string()))?;

        ensure_cloudflare_success(response, DeploymentError::CloudflarePages).await
    }

    async fn trigger_pages_deployment(
        &self,
        project_name: &str,
    ) -> Result<PagesDeploymentStatus, DeploymentError> {
        let url = format!("{}/deployments", self.project_url(project_name));
        let response = self
            .client
            .post(url)
            .bearer_auth(&self.api_token)
            .json(&json!({}))
            .send()
            .await
            .map_err(|error| DeploymentError::CloudflarePages(error.to_string()))?;
        parse_pages_deployment_response(response).await
    }

    async fn wait_pages_deployment_success(
        &self,
        project_name: &str,
        deployment_id: &str,
        expected_commit_sha: Option<&str>,
    ) -> Result<PagesDeploymentStatus, DeploymentError> {
        for _ in 0..PAGES_DEPLOYMENT_POLL_ATTEMPTS {
            let url = format!(
                "{}/deployments/{}",
                self.project_url(project_name),
                deployment_id
            );
            let response = self
                .client
                .get(url)
                .bearer_auth(&self.api_token)
                .send()
                .await
                .map_err(|error| DeploymentError::CloudflarePages(error.to_string()))?;
            let deployment = parse_pages_deployment_response(response).await?;
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

    async fn bind_custom_domain(
        &self,
        project_name: &str,
        domain: &str,
    ) -> Result<(), DeploymentError> {
        if let Some(status) = self.get_custom_domain_status(project_name, domain).await? {
            return ensure_domain_status_not_failed(domain, &status);
        }

        let url = format!("{}/domains", self.project_url(project_name));
        let response = self
            .client
            .post(url)
            .bearer_auth(&self.api_token)
            .json(&json!({ "name": domain }))
            .send()
            .await
            .map_err(|error| DeploymentError::DomainBind(error.to_string()))?;

        ensure_cloudflare_success(response, DeploymentError::DomainBind).await?;
        self.wait_custom_domain_present(project_name, domain).await
    }

    async fn get_custom_domain_status(
        &self,
        project_name: &str,
        domain: &str,
    ) -> Result<Option<String>, DeploymentError> {
        let response = self
            .client
            .get(format!(
                "{}/domains/{}",
                self.project_url(project_name),
                domain
            ))
            .bearer_auth(&self.api_token)
            .send()
            .await
            .map_err(|error| DeploymentError::DomainBind(error.to_string()))?;
        if response.status() == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        parse_pages_domain_status_response(response).await.map(Some)
    }

    async fn wait_custom_domain_present(
        &self,
        project_name: &str,
        domain: &str,
    ) -> Result<(), DeploymentError> {
        for _ in 0..PAGES_DOMAIN_POLL_ATTEMPTS {
            if let Some(status) = self.get_custom_domain_status(project_name, domain).await? {
                return ensure_domain_status_not_failed(domain, &status);
            }
            sleep(PAGES_DOMAIN_POLL_INTERVAL).await;
        }
        Err(DeploymentError::DomainBind(format!(
            "custom domain {domain} was not visible after binding"
        )))
    }
}

#[async_trait]
impl SiteDeployer for CloudflarePagesDeployer {
    async fn deploy(
        &self,
        request: &DeploymentRequest,
    ) -> Result<DeploymentResult, DeploymentError> {
        if !request.github_token.trim().is_empty() && !request.repo_owner.trim().is_empty() {
            self.validate_cloudflare_config()?;
            ensure_github_repo(
                &self.client,
                &request.repo_owner,
                &request.repo_name,
                &request.github_token,
            )
            .await?;
            self.ensure_git_project(
                &request.cloudflare_project_name,
                &request.production_branch,
                &request.repo_owner,
                &request.repo_name,
                publish_dir_from_site_dir(&request.site_dir),
            )
            .await?;
            push_site_to_github(request).await?;
            let commit_sha = git_output(&request.site_dir, &["rev-parse", "HEAD"]).await?;
            let deployment = self
                .trigger_pages_deployment(&request.cloudflare_project_name)
                .await?;
            let deployment = self
                .wait_pages_deployment_success(
                    &request.cloudflare_project_name,
                    &deployment.id,
                    Some(commit_sha.trim()),
                )
                .await?;
            self.ensure_git_project(
                &request.cloudflare_project_name,
                &request.production_branch,
                &request.repo_owner,
                &request.repo_name,
                publish_dir_from_site_dir(&request.site_dir),
            )
            .await?;
            if let Some(domain) = request.custom_domain.as_deref() {
                self.bind_custom_domain(&request.cloudflare_project_name, domain)
                    .await?;
            }
            return Ok(DeploymentResult {
                github_owner: request.repo_owner.clone(),
                github_repo_name: request.repo_name.clone(),
                github_repo_url: format!(
                    "https://github.com/{}/{}",
                    request.repo_owner, request.repo_name
                ),
                github_commit_sha: commit_sha.trim().to_string(),
                cloudflare_project_name: request.cloudflare_project_name.clone(),
                cloudflare_pages_url: deployment.url.unwrap_or_else(|| {
                    format!("https://{}.pages.dev", request.cloudflare_project_name)
                }),
                custom_domain: request.custom_domain.clone(),
            });
        }

        self.validate_config()?;
        self.ensure_project(&request.cloudflare_project_name, &request.production_branch)
            .await?;

        let args = render_wrangler_args(
            &self.wrangler_args,
            &request.site_dir,
            &request.cloudflare_project_name,
            &request.production_branch,
        )?;
        let output = Command::new(&self.wrangler_command)
            .args(args)
            .env("CLOUDFLARE_API_TOKEN", &self.api_token)
            .env("CLOUDFLARE_ACCOUNT_ID", &self.account_id)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await
            .map_err(|error| DeploymentError::CloudflarePages(error.to_string()))?;

        if !output.status.success() {
            return Err(DeploymentError::CloudflarePages(
                String::from_utf8_lossy(&output.stderr).to_string(),
            ));
        }

        if let Some(domain) = request.custom_domain.as_deref() {
            self.bind_custom_domain(&request.cloudflare_project_name, domain)
                .await?;
        }

        Ok(DeploymentResult {
            github_owner: request.repo_owner.clone(),
            github_repo_name: request.repo_name.clone(),
            github_repo_url: request.repo_owner.clone(),
            github_commit_sha: "direct-upload".to_string(),
            cloudflare_project_name: request.cloudflare_project_name.clone(),
            cloudflare_pages_url: format!(
                "https://{}.pages.dev",
                request.cloudflare_project_name
            ),
            custom_domain: request.custom_domain.clone(),
        })
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
        ensure_github_repo(
            &self.client,
            &request.repo_owner,
            &request.repo_name,
            &request.github_token,
        )
        .await?;
        push_site_to_github(request).await?;
        let commit_sha = git_output(&request.site_dir, &["rev-parse", "HEAD"]).await?;

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
                "expected_commit_sha": commit_sha.trim(),
            }))
            .timeout(API_PAGES_DEPLOY_TIMEOUT)
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
                github_commit_sha: commit_sha.trim().to_string(),
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
