use std::{path::Path, process::Stdio};

use async_trait::async_trait;
use reqwest::{Client, StatusCode};
use serde_json::json;
use thiserror::Error;
use tokio::process::Command;

#[derive(Clone, Debug)]
pub struct DeploymentRequest {
    pub repo_owner: String,
    pub repo_name: String,
    pub github_token: String,
    pub site_dir: String,
    pub cloudflare_project_name: String,
    pub production_branch: String,
    pub custom_domain: Option<String>,
}

#[derive(Clone, Debug)]
pub struct DeploymentResult {
    pub github_repo_url: String,
    pub github_commit_sha: String,
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
        let url = format!("{}/domains", self.project_url(project_name));
        let response = self
            .client
            .post(url)
            .bearer_auth(&self.api_token)
            .json(&json!({ "name": domain }))
            .send()
            .await
            .map_err(|error| DeploymentError::DomainBind(error.to_string()))?;

        ensure_cloudflare_success(response, DeploymentError::DomainBind).await
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
            github_repo_url: request.repo_owner.clone(),
            github_commit_sha: "direct-upload".to_string(),
            cloudflare_pages_url: format!("https://{}.pages.dev", request.cloudflare_project_name),
            custom_domain: request.custom_domain.clone(),
        })
    }
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
        let repo_url = format!(
            "https://github.com/{}/{}.git",
            request.repo_owner, request.repo_name
        );
        let authed_repo_url = format!(
            "https://x-access-token:{}@github.com/{}/{}.git",
            request.github_token, request.repo_owner, request.repo_name
        );

        run_git(&request.site_dir, &["init"]).await?;
        run_git(
            &request.site_dir,
            &["checkout", "-B", &request.production_branch],
        )
        .await?;
        run_git(&request.site_dir, &["add", "."]).await?;
        run_git(
            &request.site_dir,
            &["commit", "-m", "Initial generated website"],
        )
        .await?;
        run_git(
            &request.site_dir,
            &["remote", "add", "origin", &authed_repo_url],
        )
        .await?;
        run_git(
            &request.site_dir,
            &["push", "-u", "origin", &request.production_branch],
        )
        .await?;

        let commit_sha = git_output(&request.site_dir, &["rev-parse", "HEAD"]).await?;

        Ok(DeploymentResult {
            github_repo_url: repo_url,
            github_commit_sha: commit_sha.trim().to_string(),
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

pub struct DryRunDeployer;

#[async_trait]
impl SiteDeployer for DryRunDeployer {
    async fn deploy(
        &self,
        request: &DeploymentRequest,
    ) -> Result<DeploymentResult, DeploymentError> {
        Ok(DeploymentResult {
            github_repo_url: format!(
                "https://github.com/{}/{}",
                request.repo_owner, request.repo_name
            ),
            github_commit_sha: "dry-run".to_string(),
            cloudflare_pages_url: format!("https://{}.pages.dev", request.cloudflare_project_name),
            custom_domain: request.custom_domain.clone(),
        })
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
}
