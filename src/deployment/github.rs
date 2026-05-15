use std::{path::Path, process::Stdio};

use reqwest::{Client, StatusCode};
use serde_json::json;
use tokio::fs;
use tokio::process::Command;

use super::{DeploymentError, DeploymentRequest};

// ---- Git helpers ----

pub(crate) async fn run_git(
    dir: impl AsRef<Path>,
    args: &[&str],
    envs: &[(&str, String)],
) -> Result<(), DeploymentError> {
    let mut command = Command::new("git");

    command
        .args(args)
        .current_dir(dir)
        .env("GIT_TERMINAL_PROMPT", "0")
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
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        Err(DeploymentError::GitPush(format!(
            "git {:?} failed with status {:?}\nstdout:\n{}\nstderr:\n{}",
            args,
            output.status.code(),
            stdout,
            stderr,
        )))
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
    let owner = owner.trim();
    let repo = repo.trim();
    let token = token.trim();

    if owner.is_empty() || repo.is_empty() || token.is_empty() {
        return Err(DeploymentError::MissingConfig(
            "GITHUB_OWNER, generated repo name, and GITHUB_TOKEN".to_string(),
        ));
    }

    let login = github_authenticated_login(client, token).await?;

    if !login.eq_ignore_ascii_case(owner) {
        return Err(DeploymentError::GithubCreate(format!(
            "GITHUB_OWNER must match the authenticated GitHub user. GITHUB_OWNER={owner}, token user={login}"
        )));
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

        return Err(DeploymentError::GithubCreate(format!(
            "get repo failed: {status}: {body}"
        )));
    }

    let create_response = github_request(client.post("https://api.github.com/user/repos"), token)
        .json(&json!({
            "name": repo,
            "private": true,
            "auto_init": false,
        }))
        .send()
        .await
        .map_err(|error| DeploymentError::GithubCreate(error.to_string()))?;

    if create_response.status().is_success() {
        Ok(())
    } else {
        let status = create_response.status();
        let body = create_response.text().await.unwrap_or_default();

        Err(DeploymentError::GithubCreate(format!(
            "create repo failed: {status}: {body}"
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

    run_git(&request.site_dir, &["init"], &[]).await?;
    run_git(
        &request.site_dir,
        &["config", "user.email", "bot@mem-bot.local"],
        &[],
    )
    .await?;
    run_git(&request.site_dir, &["config", "user.name", "Meme Bot"], &[]).await?;
    run_git(
        &request.site_dir,
        &["checkout", "-B", &request.production_branch],
        &[],
    )
    .await?;
    ensure_site_gitignore(&request.site_dir).await?;
    run_git(&request.site_dir, &["add", "."], &[]).await?;
    // Allow retries: if nothing changed since last commit, skip commit
    let status_output = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(&request.site_dir)
        .output()
        .await
        .map_err(|e| DeploymentError::GitPush(e.to_string()))?;
    let has_changes = !status_output.stdout.is_empty();
    if has_changes {
        run_git(
            &request.site_dir,
            &["commit", "-m", "Deploy generated website"],
            &[],
        )
        .await?;
    }
    let _ = run_git(&request.site_dir, &["remote", "remove", "origin"], &[]).await;
    let auth_header = format!("AUTHORIZATION: bearer {}", request.github_token);
    let refspec = format!("HEAD:{}", request.production_branch);
    run_git(
        &request.site_dir,
        &["push", "--force", &repo_url, &refspec],
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

async fn ensure_site_gitignore(site_dir: impl AsRef<Path>) -> Result<(), DeploymentError> {
    let path = site_dir.as_ref().join(".gitignore");
    let mut existing = match fs::read_to_string(&path).await {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => {
            return Err(DeploymentError::GitPush(format!(
                "read generated site .gitignore failed: {error}"
            )));
        }
    };

    let mut changed = false;
    for entry in ["agent-prompt.md", ".agent_done.json"] {
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
        fs::write(&path, existing).await.map_err(|error| {
            DeploymentError::GitPush(format!("write generated site .gitignore failed: {error}"))
        })?;
    }
    Ok(())
}
