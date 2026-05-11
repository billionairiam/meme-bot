use std::{fs, path::Path, process::Stdio};

use reqwest::{Client, StatusCode};
use serde_json::json;
use tokio::process::Command;

use super::{DeploymentError, DeploymentRequest, DeploymentResult};

// ---- Git helpers ----

pub(crate) async fn run_git(dir: impl AsRef<Path>, args: &[&str]) -> Result<(), DeploymentError> {
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

pub(crate) async fn run_git_with_env(
    dir: impl AsRef<Path>,
    args: &[&str],
    envs: &[(&str, String)],
) -> Result<(), DeploymentError> {
    let mut command = Command::new("git");
    command
        .args(args)
        .current_dir(dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in envs {
        command.env(key, value);
    }
    let output = command
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

pub(crate) async fn run_git_allow_nothing_to_commit(
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

pub(crate) async fn git_output(
    dir: impl AsRef<Path>,
    args: &[&str],
) -> Result<String, DeploymentError> {
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

// ---- GitHub API helpers ----

pub(crate) fn github_request(
    builder: reqwest::RequestBuilder,
    token: &str,
) -> reqwest::RequestBuilder {
    builder
        .bearer_auth(token)
        .header("User-Agent", "meme-bot")
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
}

pub(crate) async fn github_authenticated_login(
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

pub(crate) async fn ensure_github_repo(
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

pub(crate) async fn push_site_to_github(
    request: &DeploymentRequest,
) -> Result<(), DeploymentError> {
    let repo_url = format!(
        "https://github.com/{}/{}.git",
        request.repo_owner, request.repo_name
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
    let _ = run_git(&request.site_dir, &["remote", "remove", "origin"]).await;
    let auth_header = format!("AUTHORIZATION: bearer {}", request.github_token);
    let refspec = format!("HEAD:{}", request.production_branch);
    run_git_with_env(
        &request.site_dir,
        &["push", &repo_url, &refspec],
        &[
            ("GIT_CONFIG_COUNT", "1".to_string()),
            (
                "GIT_CONFIG_KEY_0",
                "http.https://github.com/.extraheader".to_string(),
            ),
            ("GIT_CONFIG_VALUE_0", auth_header),
        ],
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
    for entry in ["agent_prompt.md", "agent-promt.md", ".agent_done.json"] {
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

// ---- GitCliCloudflareDeployer ----

use async_trait::async_trait;
use super::SiteDeployer;

#[allow(dead_code)]
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
            cloudflare_pages_url: format!(
                "https://{}.pages.dev",
                request.cloudflare_project_name
            ),
            custom_domain: request.custom_domain.clone(),
        })
    }
}
