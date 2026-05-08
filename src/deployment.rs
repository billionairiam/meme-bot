use std::{fs, path::Path, process::Stdio, time::Duration};

use async_trait::async_trait;
use reqwest::{Client, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::json;
use thiserror::Error;
use tokio::process::Command;
use tokio::time::sleep;

const PAGES_DEPLOYMENT_POLL_ATTEMPTS: usize = 36;
const PAGES_DEPLOYMENT_POLL_INTERVAL: Duration = Duration::from_secs(5);
const PAGES_DOMAIN_POLL_ATTEMPTS: usize = 12;
const PAGES_DOMAIN_POLL_INTERVAL: Duration = Duration::from_secs(5);
const API_PAGES_DEPLOY_TIMEOUT: Duration = Duration::from_secs(300);

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DeploymentRequest {
    pub repo_owner: String,
    pub repo_name: String,
    pub github_token: String,
    pub site_dir: String,
    pub cloudflare_project_name: String,
    pub production_branch: String,
    pub custom_domain: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DeploymentResult {
    pub github_owner: String,
    pub github_repo_name: String,
    pub github_repo_url: String,
    pub github_commit_sha: String,
    pub cloudflare_project_name: String,
    pub cloudflare_pages_url: String,
    pub custom_domain: Option<String>,
}

#[derive(Debug, Error)]
pub enum DeploymentError {
    #[error("missing deployment configuration: {0}")]
    MissingConfig(String),
    #[error("github repo creation failed: {0}")]
    GithubCreate(String),
    #[error("git push failed: {0}")]
    GitPush(String),
    #[error("cloudflare pages failed: {0}")]
    CloudflarePages(String),
    #[error("custom domain bind failed: {0}")]
    DomainBind(String),
}

impl DeploymentError {
    pub fn stage(&self) -> &'static str {
        match self {
            Self::GithubCreate(_) => "github_create",
            Self::GitPush(_) => "git_push",
            Self::CloudflarePages(_) => "pages_create",
            Self::DomainBind(_) => "domain_bind",
            Self::MissingConfig(_) => "pages_deploy",
        }
    }
}

#[async_trait]
pub trait SiteDeployer: Send + Sync {
    async fn deploy(
        &self,
        request: &DeploymentRequest,
    ) -> Result<DeploymentResult, DeploymentError>;
}

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

    fn project_url(&self, project_name: &str) -> String {
        format!(
            "https://api.cloudflare.com/client/v4/accounts/{}/pages/projects/{}",
            self.account_id, project_name
        )
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
            let deployment = self
                .trigger_pages_deployment(&request.cloudflare_project_name)
                .await?;
            let deployment = self
                .wait_pages_deployment_success(&request.cloudflare_project_name, &deployment.id)
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
            let commit_sha = git_output(&request.site_dir, &["rev-parse", "HEAD"]).await?;
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
            cloudflare_pages_url: format!("https://{}.pages.dev", request.cloudflare_project_name),
            custom_domain: request.custom_domain.clone(),
        })
    }
}

impl CloudflarePagesDeployer {
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
                &body,
                github_owner,
                github_repo,
                branch,
                publish_dir,
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
                Some("success") => return Ok(deployment),
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

#[derive(Debug)]
struct PagesDeploymentStatus {
    id: String,
    url: Option<String>,
    stage_status: Option<String>,
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

fn response_success(body: &str) -> Option<bool> {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|value| value.get("success").and_then(|success| success.as_bool()))
}

fn verify_existing_pages_project(
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

    Ok(PagesDeploymentStatus {
        id,
        url,
        stage_status,
    })
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

fn ensure_domain_status_not_failed(domain: &str, status: &str) -> Result<(), DeploymentError> {
    match status {
        "active" | "pending" | "initializing" => Ok(()),
        "blocked" | "error" | "deactivated" => Err(DeploymentError::DomainBind(format!(
            "custom domain {domain} has Cloudflare status {status}"
        ))),
        other => Err(DeploymentError::DomainBind(format!(
            "custom domain {domain} has unknown Cloudflare status {other}"
        ))),
    }
}

fn publish_dir_from_site_dir(site_dir: &str) -> &'static str {
    let root_index = Path::new(site_dir).join("index.html");
    if root_index.exists() { "/" } else { "dist" }
}

fn render_wrangler_args(
    args_template: &str,
    site_dir: &str,
    project_name: &str,
    branch: &str,
) -> Result<Vec<String>, DeploymentError> {
    let rendered = args_template
        .replace("{site_dir}", site_dir)
        .replace("{project_name}", project_name)
        .replace("{branch}", branch);

    shell_words::split(&rendered).map_err(|error| {
        DeploymentError::CloudflarePages(format!("invalid wrangler args: {error}"))
    })
}

pub struct GitCliCloudflareDeployer;

#[async_trait]
impl SiteDeployer for GitCliCloudflareDeployer {
    async fn deploy(
        &self,
        request: &DeploymentRequest,
    ) -> Result<DeploymentResult, DeploymentError> {
        let client = Client::new();
        ensure_github_repo(
            &client,
            &request.repo_owner,
            &request.repo_name,
            &request.github_token,
        )
        .await?;
        push_site_to_github(request).await?;
        let commit_sha = git_output(&request.site_dir, &["rev-parse", "HEAD"]).await?;

        Ok(DeploymentResult {
            github_owner: request.repo_owner.clone(),
            github_repo_name: request.repo_name.clone(),
            github_repo_url: format!(
                "https://github.com/{}/{}.git",
                request.repo_owner, request.repo_name
            ),
            github_commit_sha: commit_sha.trim().to_string(),
            cloudflare_project_name: request.cloudflare_project_name.clone(),
            cloudflare_pages_url: format!("https://{}.pages.dev", request.cloudflare_project_name),
            custom_domain: request.custom_domain.clone(),
        })
    }
}

async fn run_git(dir: impl AsRef<Path>, args: &[&str]) -> Result<(), DeploymentError> {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .map_err(|error| DeploymentError::GitPush(error.to_string()))?;

    if output.status.success() {
        Ok(())
    } else {
        Err(DeploymentError::GitPush(
            String::from_utf8_lossy(&output.stderr).to_string(),
        ))
    }
}

async fn run_git_allow_nothing_to_commit(
    dir: impl AsRef<Path>,
    args: &[&str],
) -> Result<(), DeploymentError> {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .map_err(|error| DeploymentError::GitPush(error.to_string()))?;

    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    if stderr.contains("nothing to commit") {
        Ok(())
    } else {
        Err(DeploymentError::GitPush(stderr.to_string()))
    }
}

async fn git_output(dir: impl AsRef<Path>, args: &[&str]) -> Result<String, DeploymentError> {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .map_err(|error| DeploymentError::GitPush(error.to_string()))?;

    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    } else {
        Err(DeploymentError::GitPush(
            String::from_utf8_lossy(&output.stderr).to_string(),
        ))
    }
}

async fn ensure_github_repo(
    client: &Client,
    owner: &str,
    repo: &str,
    token: &str,
) -> Result<(), DeploymentError> {
    if owner.trim().is_empty() || repo.trim().is_empty() || token.trim().is_empty() {
        return Err(DeploymentError::MissingConfig(
            "GITHUB_OWNER, generated repo name, and GITHUB_TOKEN".to_string(),
        ));
    }

    let get_url = format!("https://api.github.com/repos/{owner}/{repo}");
    let get_response = github_request(client.get(&get_url), token)
        .send()
        .await
        .map_err(|error| DeploymentError::GithubCreate(error.to_string()))?;
    if get_response.status().is_success() {
        return Ok(());
    }
    if get_response.status() != StatusCode::NOT_FOUND {
        let status = get_response.status();
        let body = get_response.text().await.unwrap_or_default();
        return Err(DeploymentError::GithubCreate(format!("{status}: {body}")));
    }

    let body = json!({
        "name": repo,
        "private": false,
        "auto_init": false,
    });
    let org_url = format!("https://api.github.com/orgs/{owner}/repos");
    let org_response = github_request(client.post(org_url), token)
        .json(&body)
        .send()
        .await
        .map_err(|error| DeploymentError::GithubCreate(error.to_string()))?;
    if org_response.status().is_success() {
        return Ok(());
    }

    let org_status = org_response.status();
    let org_body = org_response.text().await.unwrap_or_default();
    let login = github_authenticated_login(client, token).await?;
    if !login.eq_ignore_ascii_case(owner) {
        return Err(DeploymentError::GithubCreate(format!(
            "cannot create repo for owner {owner}; token user is {login} and org create failed: {org_status}: {org_body}"
        )));
    }

    let user_response = github_request(client.post("https://api.github.com/user/repos"), token)
        .json(&body)
        .send()
        .await
        .map_err(|error| DeploymentError::GithubCreate(error.to_string()))?;
    if user_response.status().is_success() {
        Ok(())
    } else {
        let user_status = user_response.status();
        let user_body = user_response.text().await.unwrap_or_default();
        Err(DeploymentError::GithubCreate(format!(
            "org repo create failed: {org_status}: {org_body}; user repo create failed: {user_status}: {user_body}"
        )))
    }
}

fn github_request(builder: reqwest::RequestBuilder, token: &str) -> reqwest::RequestBuilder {
    builder
        .bearer_auth(token)
        .header("User-Agent", "meme-bot")
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
}

async fn github_authenticated_login(
    client: &Client,
    token: &str,
) -> Result<String, DeploymentError> {
    let response = github_request(client.get("https://api.github.com/user"), token)
        .send()
        .await
        .map_err(|error| DeploymentError::GithubCreate(error.to_string()))?;
    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|error| DeploymentError::GithubCreate(error.to_string()))?;
    if !status.is_success() {
        return Err(DeploymentError::GithubCreate(format!("{status}: {body}")));
    }
    serde_json::from_str::<serde_json::Value>(&body)
        .ok()
        .and_then(|value| {
            value
                .get("login")
                .and_then(|login| login.as_str())
                .map(String::from)
        })
        .ok_or_else(|| DeploymentError::GithubCreate("missing authenticated login".to_string()))
}

async fn push_site_to_github(request: &DeploymentRequest) -> Result<(), DeploymentError> {
    let authed_repo_url = format!(
        "https://x-access-token:{}@github.com/{}/{}.git",
        request.github_token, request.repo_owner, request.repo_name
    );

    run_git(&request.site_dir, &["init"]).await?;
    run_git(
        &request.site_dir,
        &["config", "user.email", "bot@mem-bot.local"],
    )
    .await?;
    run_git(&request.site_dir, &["config", "user.name", "Meme Bot"]).await?;
    run_git(
        &request.site_dir,
        &["checkout", "-B", &request.production_branch],
    )
    .await?;
    ensure_site_gitignore(&request.site_dir).await?;
    run_git(&request.site_dir, &["add", "."]).await?;
    run_git_allow_nothing_to_commit(
        &request.site_dir,
        &["commit", "-m", "Deploy generated website"],
    )
    .await?;
    if run_git(
        &request.site_dir,
        &["remote", "set-url", "origin", &authed_repo_url],
    )
    .await
    .is_err()
    {
        run_git(
            &request.site_dir,
            &["remote", "add", "origin", &authed_repo_url],
        )
        .await?;
    }
    run_git(
        &request.site_dir,
        &["push", "-u", "origin", &request.production_branch],
    )
    .await
}

async fn ensure_site_gitignore(site_dir: &str) -> Result<(), DeploymentError> {
    let path = Path::new(site_dir).join(".gitignore");
    let mut existing = match fs::read_to_string(&path) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => {
            return Err(DeploymentError::GitPush(format!(
                "read generated site .gitignore failed: {error}"
            )));
        }
    };

    let mut changed = false;
    for entry in ["agent_prompt.md", ".agent_done.json"] {
        if !existing.lines().any(|line| line.trim() == entry) {
            if !existing.is_empty() && !existing.ends_with('\n') {
                existing.push('\n');
            }
            existing.push_str(entry);
            existing.push('\n');
            changed = true;
        }
    }

    if changed {
        fs::write(&path, existing).map_err(|error| {
            DeploymentError::GitPush(format!("write generated site .gitignore failed: {error}"))
        })?;
    }
    Ok(())
}

pub struct DryRunDeployer;

#[async_trait]
impl SiteDeployer for DryRunDeployer {
    async fn deploy(
        &self,
        request: &DeploymentRequest,
    ) -> Result<DeploymentResult, DeploymentError> {
        Ok(DeploymentResult {
            github_owner: request.repo_owner.clone(),
            github_repo_name: request.repo_name.clone(),
            github_repo_url: format!(
                "https://github.com/{}/{}",
                request.repo_owner, request.repo_name
            ),
            github_commit_sha: "dry-run".to_string(),
            cloudflare_project_name: request.cloudflare_project_name.clone(),
            cloudflare_pages_url: format!("https://{}.pages.dev", request.cloudflare_project_name),
            custom_domain: request.custom_domain.clone(),
        })
    }
}

// ---- HttpClientPagesDeployer ----

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

#[derive(Debug, Deserialize)]
struct PagesDeployResponse {
    cloudflare_pages_url: String,
    #[serde(default)]
    custom_domain: Option<String>,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_wrangler_args() {
        let args = render_wrangler_args(
            "wrangler pages deploy {site_dir} --project-name={project_name} --branch={branch}",
            "./dist",
            "pepe-site",
            "main",
        )
        .unwrap();
        assert_eq!(args[0], "wrangler");
        assert_eq!(args[4], "--project-name=pepe-site");
    }

    #[test]
    fn parses_cloudflare_success_field() {
        assert_eq!(response_success(r#"{"success":true}"#), Some(true));
        assert_eq!(response_success(r#"{"success":false}"#), Some(false));
    }

    #[test]
    fn verifies_existing_pages_project_source() {
        let body = r#"{
            "success": true,
            "result": {
                "source": {
                    "config": {
                        "owner": "memebot",
                        "repo_name": "example.com",
                        "production_branch": "main"
                    }
                },
                "build_config": {
                    "destination_dir": "/"
                }
            }
        }"#;

        assert!(verify_existing_pages_project(body, "memebot", "example.com", "main", "/").is_ok());
        assert!(verify_existing_pages_project(body, "memebot", "wrong.com", "main", "/").is_err());
    }

    #[test]
    fn rejects_failed_pages_domain_statuses() {
        assert!(ensure_domain_status_not_failed("example.com", "active").is_ok());
        assert!(ensure_domain_status_not_failed("example.com", "pending").is_ok());
        assert!(ensure_domain_status_not_failed("example.com", "initializing").is_ok());
        assert!(ensure_domain_status_not_failed("example.com", "error").is_err());
        assert!(ensure_domain_status_not_failed("example.com", "blocked").is_err());
    }

    #[test]
    fn selects_publish_dir_from_generated_site() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("index.html"), "<html></html>").unwrap();
        assert_eq!(
            publish_dir_from_site_dir(&dir.path().display().to_string()),
            "/"
        );

        let dist_only = tempfile::tempdir().unwrap();
        fs::create_dir_all(dist_only.path().join("dist")).unwrap();
        fs::write(dist_only.path().join("dist/index.html"), "<html></html>").unwrap();
        assert_eq!(
            publish_dir_from_site_dir(&dist_only.path().display().to_string()),
            "dist"
        );
    }
}
