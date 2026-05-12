use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::domain::DomainError;

pub mod cloudflare;
mod github;

pub use cloudflare::{CloudflarePagesDeployer, HttpClientPagesDeployer};
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

impl From<DeploymentError> for DomainError {
    fn from(error: DeploymentError) -> Self {
        match error {
            DeploymentError::MissingConfig(msg) => DomainError::MissingConfig(msg),
            other => DomainError::Cloudflare(other.to_string()),
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

#[cfg(test)]
mod tests {
    use super::cloudflare::*;
    use std::fs;

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
    fn extracts_cloudflare_pages_commit_hash() {
        let deployment = serde_json::json!({
            "deployment_trigger": {
                "metadata": {
                    "commit_hash": "abc123"
                }
            }
        });
        assert_eq!(
            extract_deployment_commit_hash(&deployment),
            Some("abc123".to_string())
        );
    }

    #[test]
    fn rejects_mismatched_cloudflare_pages_commit_hash() {
        let deployment = crate::deployment::cloudflare::PagesDeploymentStatus {
            id: "dep_1".to_string(),
            url: None,
            stage_status: Some("success".to_string()),
            commit_hash: Some("abc123".to_string()),
        };
        assert!(verify_deployment_commit(&deployment, Some("abc123")).is_ok());
        assert!(verify_deployment_commit(&deployment, Some("def456")).is_err());
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
