use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use regex::Regex;
use thiserror::Error;
use tokio::{process::Command, time::timeout};
use uuid::Uuid;

use crate::{
    config::{Config, GenerationProviderName},
    models::WebsiteInput,
    validation::DEFAULT_CONTRACT_ADDRESS,
};

const AGENT_PROMPT_FILE_NAME: &str = "agent-prompt.md";

#[derive(Clone, Debug)]
pub struct SiteGenerationRequest {
    pub order_id: Uuid,
    pub build_mode: String,
    pub template_local_path: Option<String>,
    pub input: WebsiteInput,
    pub output_dir: PathBuf,
}

#[derive(Clone, Debug)]
pub struct SiteGenerationResult {
    pub provider_name: String,
    pub output_dir: PathBuf,
}

#[derive(Debug, Error)]
pub enum GenerationError {
    #[error("provider quota exhausted")]
    QuotaExhausted,
    #[error("provider rate limited")]
    RateLimited,
    #[error("provider unavailable")]
    ProviderUnavailable,
    #[error("invalid generation input: {0}")]
    InvalidInput(String),
    #[error("invalid generated site: {0}")]
    InvalidOutput(String),
    #[error("generation failed: {0}")]
    Failed(String),
}

impl GenerationError {
    pub fn is_provider_recoverable(&self) -> bool {
        matches!(
            self,
            Self::QuotaExhausted
                | Self::RateLimited
                | Self::ProviderUnavailable
                | Self::InvalidOutput(_)
        )
    }
}

struct CliProvider {
    name: &'static str,
    command: String,
    args_template: String,
    timeout: Duration,
}

pub struct GenerationRouter {
    providers: Vec<CliProvider>,
}

impl GenerationRouter {
    pub fn from_config(config: &Config) -> Self {
        let timeout = Duration::from_secs(config.agent_timeout_seconds);
        let gemini = CliProvider {
            name: "gemini",
            command: config.gemini_agent_command.clone(),
            args_template: config.gemini_agent_args.clone(),
            timeout,
        };
        let codex = CliProvider {
            name: "codex",
            command: config.codex_agent_command.clone(),
            args_template: config.codex_agent_args.clone(),
            timeout,
        };
        let claude = CliProvider {
            name: "claude",
            command: config.claude_agent_command.clone(),
            args_template: config.claude_agent_args.clone(),
            timeout,
        };
        let providers = match config.generation_primary_provider {
            GenerationProviderName::Gemini => vec![gemini, codex, claude],
            GenerationProviderName::Codex => vec![codex, gemini, claude],
            GenerationProviderName::Claude => vec![claude, gemini, codex],
        };
        Self { providers }
    }

    pub async fn generate(
        &self,
        request: &SiteGenerationRequest,
    ) -> Result<SiteGenerationResult, GenerationError> {
        prepare_output_dir(&request.output_dir)?;
        copy_logo_to_output_dir(request)?;

        let mut last_error = None;
        let total = self.providers.len();
        for (i, provider) in self.providers.iter().enumerate() {
            tracing::info!(
                provider = provider.name,
                index = i + 1,
                total,
                order_id = %request.order_id,
                "starting generation with provider"
            );
            match run_cli_provider(provider, request).await {
                Ok(result) => {
                    tracing::info!(
                        provider = provider.name,
                        order_id = %request.order_id,
                        "generation succeeded"
                    );
                    return Ok(result);
                }
                Err(error) if error.is_provider_recoverable() => {
                    tracing::warn!(
                        provider = provider.name,
                        order_id = %request.order_id,
                        error = %error,
                        "provider failed (recoverable), switching to next provider"
                    );
                    last_error = Some(error);
                    continue;
                }
                Err(error) => {
                    tracing::error!(
                        provider = provider.name,
                        order_id = %request.order_id,
                        error = %error,
                        "provider failed (non-recoverable)"
                    );
                    return Err(error);
                }
            }
        }
        tracing::error!(
            order_id = %request.order_id,
            "all {} providers exhausted", total
        );
        Err(last_error.unwrap_or(GenerationError::ProviderUnavailable))
    }
}

async fn run_cli_provider(
    provider: &CliProvider,
    request: &SiteGenerationRequest,
) -> Result<SiteGenerationResult, GenerationError> {
    let prompt = agent_prompt(request, provider.name)?;
    let prompt_file = request.output_dir.join(AGENT_PROMPT_FILE_NAME);
    fs::write(&prompt_file, &prompt).map_err(|error| {
        GenerationError::Failed(format!("write agent prompt failed: {error}"))
    })?;
    let done_file = request.output_dir.join(".agent_done.json");
    match fs::remove_file(&done_file) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(GenerationError::Failed(format!(
                "remove stale done file failed: {error}"
            )));
        }
    }

    let args = render_agent_args(&provider.args_template, &prompt)?;

    tracing::info!(
        provider = provider.name,
        command = %provider.command,
        output_dir = %request.output_dir.display(),
        timeout_secs = provider.timeout.as_secs(),
        "launching CLI agent"
    );

    let mut command = Command::new(&provider.command);
    command
        .args(args)
        .current_dir(&request.output_dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let output = timeout(provider.timeout, command.output())
        .await
        .map_err(|_| GenerationError::ProviderUnavailable)?
        .map_err(|error| map_provider_spawn_error(error, provider.name))?;

    if output.status.success() {
        validate_agent_output(request, provider.name, &done_file)?;
        Ok(SiteGenerationResult {
            provider_name: provider.name.to_string(),
            output_dir: request.output_dir.clone(),
        })
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).to_ascii_lowercase();
        if stderr.contains("quota") || stderr.contains("limit") {
            Err(GenerationError::QuotaExhausted)
        } else if stderr.contains("rate") {
            Err(GenerationError::RateLimited)
        } else {
            Err(GenerationError::Failed(stderr))
        }
    }
}

fn agent_prompt(
    request: &SiteGenerationRequest,
    provider_name: &str,
) -> Result<String, GenerationError> {
    let spec = serde_json::json!({
        "order_id": request.order_id,
        "provider": provider_name,
        "build_mode": request.build_mode,
        "template_local_path": request.template_local_path,
        "input": request.input,
        "output_dir": request.output_dir,
        "done_file": request.output_dir.join(".agent_done.json"),
        "constraints": {
            "static_site_only": true,
            "no_backend": true,
            "no_database": true,
            "no_user_prompt": true,
            "do_not_ask_questions": true,
            "write_files_directly": true
        }
    });

    let token_name = &request.input.token_name;
    let logo_src = Path::new(&request.input.logo_asset_path);
    let has_logo = !request.input.logo_asset_path.is_empty() && logo_src.exists();
    let logo_note = if has_logo {
        let ext = logo_src
            .extension()
            .map(|e| format!(".{}", e.to_string_lossy()))
            .unwrap_or_else(|| ".png".to_string());
        format!(
            "A logo image is provided at `./logo{ext}`. Use it as the primary visual anchor — place it prominently in the hero section and favicon. Inspect the logo's visual style, including colors, shape language, line weight, texture, mood, and meme character energy. Build the entire website identity around that style.",
        )
    } else {
        "No logo image is available. Create a text-based or emoji-based logo/token icon that fits the meme theme.".to_string()
    };
    let dextools_url = optional_dextools_url(&request.input.contract_address);

    let template = load_prompt_template(provider_name)?;

    Ok(template
        .replace("{token_name}", token_name)
        .replace("{token_chain}", request.input.token_chain.display_label())
        .replace("{ticker}", &request.input.token_ticker)
        .replace("{contract}", &request.input.contract_address)
        .replace("{x_url}", &request.input.x_url)
        .replace("{tg_url}", &request.input.telegram_url)
        .replace("{dextools_url}", &dextools_url)
        .replace("{logo_note}", &logo_note)
        .replace("{color_instruction}", if has_logo {
            "Extract the dominant and accent colors from the provided logo image. Use those colors for backgrounds, buttons, headings, borders, and the custom SVG link icons. This creates a cohesive brand identity."
        } else {
            "Pick colors that match the token theme — bold, high-contrast, crypto-native. Use gradients and glow effects. Avoid muted corporate palettes."
        })
        .replace("{provider_name}", provider_name)
        .replace("{spec}", &serde_json::to_string_pretty(&spec).unwrap_or_else(|_| "{}".to_string())))
}

fn load_prompt_template(provider_name: &str) -> Result<String, GenerationError> {
    let path = format!("prompts/agent-{provider_name}.md");
    std::fs::read_to_string(&path).map_err(|error| {
        GenerationError::Failed(format!("failed to read prompt template {path}: {error}"))
    })
}

fn render_agent_args(args_template: &str, prompt: &str) -> Result<Vec<String>, GenerationError> {
    const PROMPT_PLACEHOLDER: &str = "\x1f__PROMPT__\x1f";
    let rendered = args_template.replace("{prompt}", PROMPT_PLACEHOLDER);

    let mut args: Vec<String> = shell_words::split(&rendered)
        .map_err(|error| GenerationError::Failed(format!("invalid agent args: {error}")))?;
    for arg in &mut args {
        if arg == PROMPT_PLACEHOLDER {
            *arg = prompt.to_string();
        }
    }
    Ok(args)
}

fn prepare_output_dir(output_dir: &Path) -> Result<(), GenerationError> {
    if output_dir.exists() {
        fs::remove_dir_all(output_dir).map_err(|error| {
            GenerationError::Failed(format!("clean output dir failed: {error}"))
        })?;
    }
    fs::create_dir_all(output_dir)
        .map_err(|error| GenerationError::Failed(format!("create output dir failed: {error}")))
}

/// Copy the user's logo into the generation output directory so the AI agent
/// can reference it. The prompt tells the agent the logo lives at `./logo.*`.
fn copy_logo_to_output_dir(request: &SiteGenerationRequest) -> Result<(), GenerationError> {
    let src = Path::new(&request.input.logo_asset_path);
    if !src.exists() {
        tracing::warn!(path = %request.input.logo_asset_path, "logo file not found, skipping copy");
        return Ok(());
    }
    let ext = src
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_else(|| ".png".to_string());
    let dest = request.output_dir.join(format!("logo{ext}"));
    fs::copy(src, &dest).map_err(|error| {
        GenerationError::Failed(format!("copy logo to output dir failed: {error}"))
    })?;
    tracing::info!(src = %src.display(), dest = %dest.display(), "copied logo to output dir");
    Ok(())
}

fn validate_agent_output(
    request: &SiteGenerationRequest,
    provider_name: &str,
    done_file: &Path,
) -> Result<(), GenerationError> {
    let output_dir = &request.output_dir;
    if !done_file.exists() {
        return Err(GenerationError::InvalidOutput(format!(
            "agent exited successfully but did not create {}",
            done_file.display()
        )));
    }

    let done_content = fs::read_to_string(done_file).map_err(|error| {
        GenerationError::InvalidOutput(format!("read done file failed: {error}"))
    })?;
    let done_json: serde_json::Value = serde_json::from_str(&done_content).map_err(|error| {
        GenerationError::InvalidOutput(format!("invalid done file JSON: {error}"))
    })?;
    if done_json.get("provider").and_then(|value| value.as_str()) != Some(provider_name) {
        return Err(GenerationError::InvalidOutput(
            "done file provider does not match the generation provider".to_string(),
        ));
    }
    if done_json.get("status").and_then(|value| value.as_str()) != Some("completed") {
        return Err(GenerationError::InvalidOutput(
            "done file status is not completed".to_string(),
        ));
    }

    let entrypoint = done_json
        .get("entrypoint")
        .and_then(|value| value.as_str())
        .unwrap_or("index.html");
    let entrypoint = sanitize_entrypoint(entrypoint)?;
    let entry_path = output_dir.join(&entrypoint);
    let fallback_entry = if output_dir.join("index.html").exists() {
        Some(PathBuf::from("index.html"))
    } else if output_dir.join("dist/index.html").exists() {
        Some(PathBuf::from("dist/index.html"))
    } else {
        None
    };
    let entry_path = if entry_path.exists() {
        entry_path
    } else if let Some(fallback) = fallback_entry {
        output_dir.join(fallback)
    } else {
        return Err(GenerationError::InvalidOutput(
            "agent completed but no deployable index.html or dist/index.html was found".to_string(),
        ));
    };

    validate_html_entry(output_dir, &entry_path, request)?;

    Ok(())
}

fn sanitize_entrypoint(entrypoint: &str) -> Result<PathBuf, GenerationError> {
    let path = PathBuf::from(entrypoint);
    if path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err(GenerationError::InvalidOutput(
            "done file entrypoint must stay inside the output directory".to_string(),
        ));
    }
    Ok(path)
}

fn validate_html_entry(
    output_dir: &Path,
    entry_path: &Path,
    request: &SiteGenerationRequest,
) -> Result<(), GenerationError> {
    let html = fs::read_to_string(entry_path).map_err(|error| {
        GenerationError::InvalidOutput(format!("read entrypoint failed: {error}"))
    })?;
    let normalized = html.to_ascii_lowercase();
    if normalized.trim().len() < 200 {
        return Err(GenerationError::InvalidOutput(
            "entrypoint HTML is too small to be a complete website".to_string(),
        ));
    }
    if !normalized.contains("<html") || !normalized.contains("<body") {
        return Err(GenerationError::InvalidOutput(
            "entrypoint must contain a full HTML document".to_string(),
        ));
    }

    let dextools_url = optional_dextools_url(&request.input.contract_address);
    for required in [
        request.input.token_name.as_str(),
        request.input.token_ticker.as_str(),
        request.input.contract_address.as_str(),
        request.input.x_url.as_str(),
        request.input.telegram_url.as_str(),
        dextools_url.as_str(),
    ]
    .into_iter()
    .filter(|value| !value.trim().is_empty())
    {
        if !normalized.contains(&required.to_ascii_lowercase()) {
            return Err(GenerationError::InvalidOutput(format!(
                "entrypoint is missing required content: {required}"
            )));
        }
    }

    validate_local_asset_references(output_dir, entry_path, &html)
}

fn optional_dextools_url(contract_address: &str) -> String {
    if contract_address.trim().is_empty() || contract_address.trim() == DEFAULT_CONTRACT_ADDRESS {
        String::new()
    } else {
        format!("https://www.dextools.io/app/en/token/{contract_address}")
    }
}

fn validate_local_asset_references(
    output_dir: &Path,
    entry_path: &Path,
    html: &str,
) -> Result<(), GenerationError> {
    let attr_regex = Regex::new(r#"(?:src|href)=["']([^"']+)["']"#)
        .map_err(|error| GenerationError::Failed(format!("asset regex failed: {error}")))?;
    let entry_dir = entry_path.parent().unwrap_or(output_dir);
    let mut missing = HashSet::new();

    for captures in attr_regex.captures_iter(html) {
        let Some(reference) = captures.get(1).map(|m| m.as_str().trim()) else {
            continue;
        };
        if should_skip_reference(reference) {
            continue;
        }
        let relative = reference
            .split_once('#')
            .map(|(path, _)| path)
            .unwrap_or(reference)
            .split_once('?')
            .map(|(path, _)| path)
            .unwrap_or(reference)
            .trim_start_matches('/');
        if relative.is_empty() {
            continue;
        }
        let candidate = entry_dir.join(relative);
        if !candidate.exists() {
            missing.insert(reference.to_string());
        }
    }

    if missing.is_empty() {
        Ok(())
    } else {
        let mut missing = missing.into_iter().collect::<Vec<_>>();
        missing.sort();
        Err(GenerationError::InvalidOutput(format!(
            "entrypoint references missing local assets: {}",
            missing.join(", ")
        )))
    }
}

fn should_skip_reference(reference: &str) -> bool {
    let lower = reference.to_ascii_lowercase();
    lower.starts_with("http://")
        || lower.starts_with("https://")
        || lower.starts_with("mailto:")
        || lower.starts_with("tel:")
        || lower.starts_with("data:")
        || lower.starts_with('#')
        || lower.starts_with("javascript:")
}

fn map_provider_spawn_error(error: std::io::Error, provider: &str) -> GenerationError {
    if error.kind() == std::io::ErrorKind::NotFound {
        GenerationError::ProviderUnavailable
    } else {
        GenerationError::Failed(format!("{provider}: {error}"))
    }
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use crate::models::TokenChain;

    use super::*;

    fn test_request(output_dir: PathBuf) -> SiteGenerationRequest {
        SiteGenerationRequest {
            order_id: Uuid::new_v4(),
            build_mode: "template".to_string(),
            template_local_path: None,
            input: WebsiteInput {
                token_chain: TokenChain::Eth,
                token_ticker: "MOON".to_string(),
                token_name: "Moon Token".to_string(),
                contract_address: "0x1111111111111111111111111111111111111111".to_string(),
                x_url: "https://x.com/moon".to_string(),
                telegram_url: "https://t.me/moonchat".to_string(),
                logo_asset_path: "logo.png".to_string(),
                requested_domain: None,
            },
            output_dir,
        }
    }

    #[test]
    fn rejects_missing_local_assets() {
        let dir = tempdir().unwrap();
        let request = test_request(dir.path().to_path_buf());
        let html = r#"<!doctype html>
<html><body>
  <h1>Moon Token</h1>
  <p>MOON</p>
  <p>0x1111111111111111111111111111111111111111</p>
  <a href="https://x.com/moon">X</a>
  <a href="https://t.me/moonchat">Telegram</a>
  <img src="missing.png" alt="Moon Token">
</body></html>"#;
        fs::write(dir.path().join("index.html"), html).unwrap();
        fs::write(
            dir.path().join(".agent_done.json"),
            serde_json::json!({
                "provider": "test",
                "status": "completed",
                "entrypoint": "index.html"
            })
            .to_string(),
        )
        .unwrap();

        let error = validate_agent_output(&request, "test", &dir.path().join(".agent_done.json"))
            .unwrap_err();
        assert!(matches!(error, GenerationError::InvalidOutput(_)));
    }

    #[test]
    fn html_validation_allows_missing_optional_coin_fields() {
        let dir = tempdir().unwrap();
        let request = SiteGenerationRequest {
            order_id: Uuid::new_v4(),
            build_mode: "ai_free".to_string(),
            template_local_path: None,
            input: WebsiteInput {
                token_chain: TokenChain::Eth,
                token_ticker: "LOGO".to_string(),
                token_name: "Logo Coin".to_string(),
                contract_address: DEFAULT_CONTRACT_ADDRESS.to_string(),
                x_url: crate::validation::DEFAULT_X_URL.to_string(),
                telegram_url: crate::validation::DEFAULT_TELEGRAM_URL.to_string(),
                logo_asset_path: "logo.png".to_string(),
                requested_domain: Some("logocoin.xyz".to_string()),
            },
            output_dir: dir.path().to_path_buf(),
        };
        let entry_path = dir.path().join("index.html");
        fs::write(
            &entry_path,
            r#"<!doctype html>
<html>
  <head><title>Logo Coin</title></head>
  <body>
    <main>
      <h1>Logo Coin</h1>
      <p>LOGO is a clean meme coin landing page generated from required brand inputs.</p>
      <p>0x0000000000000000000000000000000000000000</p>
      <a href="https://x.com">X</a>
      <a href="https://t.me">Telegram</a>
      <section>Hero, about, tokenomics, roadmap, and community sections with polished copy.</section>
      <section>This page uses default contract and social placeholders because the user skipped them.</section>
      <footer>This is a meme token. Nothing on this site is financial advice. Do your own research.</footer>
    </main>
  </body>
</html>"#,
        )
        .unwrap();

        validate_html_entry(dir.path(), &entry_path, &request).unwrap();
    }
}
