use std::{env, net::SocketAddr, sync::Arc, time::Duration};

use axum::{
    Router,
    extract::{Request, State},
    http::{HeaderMap, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Json, Response},
    routing::{get, post},
};
use meme_bot::{
    config::{NamecheapConfig, NamecheapContactConfig, load_env_file},
    deployment::{
        DeploymentError,
        cloudflare::{
            ensure_domain_status_not_failed,
            parse_pages_deployment_response, response_success, verify_deployment_commit,
            verify_existing_pages_project, PagesDeploymentStatus,
        },
    },
    domain::{
        ApiErrorResponse, CloudflareZoneResult, DnsProvisioner, DnsRecord, DomainError,
        DomainPurchaseRequest, DomainPurchaseResult, DomainQuote, DomainQuoteRequest,
        DomainRegistrar, NamecheapDnsProvisioner, NamecheapRegistrar,
    },
    logging::init_logging,
};
use serde::Deserialize;
use tokio::time::sleep;
use tracing::{error, info, warn};

const PAGES_DEPLOYMENT_POLL_ATTEMPTS: usize = 36;
const PAGES_DEPLOYMENT_POLL_INTERVAL: Duration = Duration::from_secs(5);
const PAGES_DOMAIN_POLL_ATTEMPTS: usize = 120;
const PAGES_DOMAIN_POLL_INTERVAL: Duration = Duration::from_secs(10);

struct AppState {
    registrar: NamecheapRegistrar,
    nc_dns: NamecheapDnsProvisioner,
    cf_account_id: String,
    cf_api_token: String,
    cf_client: reqwest::Client,
    api_key: String,
}

#[tokio::main]
async fn main() {
    let env_file =
        load_env_file("API_SERVER_ENV_FILE", ".env.api-server").expect("load api-server env file");
    let _log_guard = init_logging("api-server");
    if let Some(path) = env_file {
        info!(env_file = %path, "loaded api-server env file");
    } else {
        info!("api-server env file not found; using process environment only");
    }

    let api_key = env::var("API_SERVICE_API_KEY")
        .expect("API_SERVICE_API_KEY must be set (min 32 characters)");
    assert!(
        api_key.len() >= 32,
        "API_SERVICE_API_KEY must be at least 32 characters"
    );

    let port: u16 = env::var("API_SERVICE_PORT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(8080);
    let bind_addr = env::var("API_SERVICE_BIND_ADDR").unwrap_or_else(|_| "127.0.0.1".to_string());
    let nc_config = namecheap_config_from_env();
    let cf_account_id = env::var("CLOUDFLARE_ACCOUNT_ID").unwrap_or_default();
    let cf_api_token = env::var("CLOUDFLARE_API_TOKEN").unwrap_or_default();

    if nc_config.api_user.is_empty() {
        warn!("NAMECHEAP_API_USER not set; domain endpoints will fail with MissingConfig");
    }
    if cf_account_id.is_empty() || cf_api_token.is_empty() {
        warn!("CLOUDFLARE_ACCOUNT_ID/CLOUDFLARE_API_TOKEN not set; Cloudflare endpoints will fail");
    }

    let state = Arc::new(AppState {
        registrar: NamecheapRegistrar::new(nc_config.clone()),
        nc_dns: NamecheapDnsProvisioner::new(nc_config),
        cf_account_id,
        cf_api_token,
        cf_client: reqwest::Client::new(),
        api_key,
    });

    let app = Router::new()
        .route("/api/v1/health", get(health))
        .route("/api/v1/domain/quote", post(domain_quote))
        .route("/api/v1/domain/purchase", post(domain_purchase))
        .route("/api/v1/dns/create-zone", post(dns_create_zone))
        .route("/api/v1/dns/set-nameservers", post(dns_set_nameservers))
        .route("/api/v1/dns/set-records", post(dns_set_records))
        .route("/api/v1/pages/deploy", post(pages_deploy))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            auth_middleware,
        ))
        .with_state(state);

    let addr: SocketAddr = format!("{bind_addr}:{port}")
        .parse()
        .expect("invalid bind address");
    info!("API server starting on {addr} (rate limiting delegated to reverse proxy)");
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}

// ---- Auth middleware ----

async fn auth_middleware(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    request: Request,
    next: Next,
) -> Response {
    let path = request.uri().path();

    // Health endpoint does not need auth
    if path == "/api/v1/health" {
        return next.run(request).await;
    }

    let provided = headers
        .get("x-api-key")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    if provided != state.api_key {
        return (
            StatusCode::UNAUTHORIZED,
            api_error_body("unauthorized", "invalid API key"),
        )
            .into_response();
    }

    next.run(request).await
}

// ---- Health ----

async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({"status": "ok"}))
}

// ---- Domain ----

async fn domain_quote(
    State(state): State<Arc<AppState>>,
    Json(request): Json<DomainQuoteRequest>,
) -> Result<Json<DomainQuote>, ApiError> {
    info!(domain = %request.domain, "domain quote");
    state
        .registrar
        .quote(&request)
        .await
        .map(Json)
        .map_err(ApiError)
}

async fn domain_purchase(
    State(state): State<Arc<AppState>>,
    Json(request): Json<DomainPurchaseRequest>,
) -> Result<Json<DomainPurchaseResult>, ApiError> {
    info!(domain = %request.domain, "domain purchase");
    state
        .registrar
        .purchase(&request)
        .await
        .map(Json)
        .map_err(ApiError)
}

// ---- DNS ----

#[derive(Deserialize)]
struct CreateZoneRequest {
    domain: String,
}

#[derive(Deserialize)]
struct SetNameserversRequest {
    domain: String,
    nameservers: Vec<String>,
}

#[derive(Deserialize)]
struct SetRecordsRequest {
    domain: String,
    records: Vec<DnsRecord>,
}

async fn dns_create_zone(
    State(state): State<Arc<AppState>>,
    Json(request): Json<CreateZoneRequest>,
) -> Result<Json<CloudflareZoneResult>, ApiError> {
    info!(domain = %request.domain, "create zone");

    if state.cf_account_id.is_empty() || state.cf_api_token.is_empty() {
        return Err(ApiError(DomainError::MissingConfig(
            "CLOUDFLARE_ACCOUNT_ID and CLOUDFLARE_API_TOKEN".to_string(),
        )));
    }

    let response = state
        .cf_client
        .post("https://api.cloudflare.com/client/v4/zones")
        .bearer_auth(&state.cf_api_token)
        .json(&serde_json::json!({
            "name": request.domain,
            "account": { "id": state.cf_account_id },
            "jump_start": true,
        }))
        .send()
        .await
        .map_err(|e| DomainError::Cloudflare(e.to_string()))?;

    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|e| DomainError::Cloudflare(e.to_string()))?;

    if !status.is_success() {
        return Err(DomainError::Cloudflare(format!("{status}: {body}")))?;
    }

    let json: serde_json::Value = serde_json::from_str(&body)
        .map_err(|e| DomainError::Cloudflare(format!("parse response: {e}")))?;

    if json.get("success").and_then(|v| v.as_bool()) != Some(true) {
        let errors = json
            .get("errors")
            .map(|e| e.to_string())
            .unwrap_or_else(|| "unknown error".to_string());
        return Err(DomainError::Cloudflare(format!(
            "zone creation failed: {errors}"
        )))?;
    }

    let result = json
        .get("result")
        .ok_or_else(|| DomainError::Cloudflare("missing result".to_string()))?;

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

    Ok(Json(CloudflareZoneResult {
        zone_id,
        nameservers,
    }))
}

async fn dns_set_nameservers(
    State(state): State<Arc<AppState>>,
    Json(request): Json<SetNameserversRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    info!(domain = %request.domain, "set nameservers");
    state
        .nc_dns
        .set_namecheap_nameservers(&request.domain, &request.nameservers)
        .await
        .map(|_| Json(serde_json::json!({})))
        .map_err(ApiError)
}

async fn dns_set_records(
    State(state): State<Arc<AppState>>,
    Json(request): Json<SetRecordsRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    info!(domain = %request.domain, "set dns records");
    state
        .nc_dns
        .set_dns_records(&request.domain, &request.records)
        .await
        .map(|_| Json(serde_json::json!({})))
        .map_err(ApiError)
}

// ---- Cloudflare Pages (Git integration) ----

#[derive(Deserialize)]
struct PagesDeployRequest {
    project_name: String,
    github_owner: String,
    github_repo: String,
    production_branch: String,
    #[serde(default = "default_pages_publish_dir")]
    publish_dir: String,
    #[serde(default)]
    custom_domain: Option<String>,
    #[serde(default)]
    expected_commit_sha: Option<String>,
}

fn default_pages_publish_dir() -> String {
    "/".to_string()
}

async fn pages_deploy(
    State(state): State<Arc<AppState>>,
    Json(request): Json<PagesDeployRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    info!(
        project = %request.project_name,
        repo = format!("{}/{}", request.github_owner, request.github_repo),
        "pages deploy"
    );

    if state.cf_account_id.is_empty() || state.cf_api_token.is_empty() {
        return Err(ApiError(DomainError::MissingConfig(
            "CLOUDFLARE_ACCOUNT_ID and CLOUDFLARE_API_TOKEN".to_string(),
        )));
    }

    let cf_url = format!(
        "https://api.cloudflare.com/client/v4/accounts/{}/pages/projects",
        state.cf_account_id
    );

    let project_url = format!("{cf_url}/{}", request.project_name);
    let existing = state
        .cf_client
        .get(&project_url)
        .bearer_auth(&state.cf_api_token)
        .send()
        .await
        .map_err(|e| DomainError::Cloudflare(e.to_string()))?;
    let project_exists = existing.status().is_success();
    if project_exists {
        let body = existing
            .text()
            .await
            .map_err(|e| DomainError::Cloudflare(e.to_string()))?;
        verify_existing_pages_project(
            &body,
            &request.github_owner,
            &request.github_repo,
            &request.production_branch,
            &request.publish_dir,
        )?;
    } else if existing.status() != StatusCode::NOT_FOUND {
        let status = existing.status();
        let body = existing
            .text()
            .await
            .map_err(|e| DomainError::Cloudflare(e.to_string()))?;
        return Err(DomainError::Cloudflare(format!(
            "get project failed: {status}: {body}"
        )))?;
    }

    if !project_exists {
        // Create Git-connected Pages project
        let response = state
            .cf_client
            .post(&cf_url)
            .bearer_auth(&state.cf_api_token)
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
            .map_err(|e| DomainError::Cloudflare(e.to_string()))?;

        let status = response.status();
        let body = response
            .text()
            .await
            .map_err(|e| DomainError::Cloudflare(e.to_string()))?;

        if !status.is_success() {
            error!(status = %status, body = %body, "cloudflare pages project creation failed");
            return Err(DomainError::Cloudflare(format!(
                "create pages project failed: {status}: {body}"
            )))?;
        }

        let json: serde_json::Value = serde_json::from_str(&body)
            .map_err(|e| DomainError::Cloudflare(format!("parse response: {e}")))?;

        if json.get("success").and_then(|v| v.as_bool()) != Some(true) {
            let errors = json
                .get("errors")
                .map(|e| e.to_string())
                .unwrap_or_else(|| "unknown error".to_string());
            return Err(DomainError::Cloudflare(format!(
                "pages project creation failed: {errors}"
            )))?;
        }
    }

    let deployment = trigger_pages_deployment(&state, &request.project_name).await?;
    let deployment = wait_pages_deployment_success(
        &state,
        &request.project_name,
        &deployment.id,
        request.expected_commit_sha.as_deref(),
    )
    .await?;

    // Bind custom domain if requested
    if let Some(domain) = &request.custom_domain {
        info!(domain = %domain, project = %request.project_name, "binding custom domain");
        bind_pages_custom_domain(&state, &request.project_name, domain).await?;
    }

    let pages_url = deployment
        .url
        .unwrap_or_else(|| format!("https://{}.pages.dev", request.project_name));
    info!(url = %pages_url, "pages project created successfully");

    Ok(Json(serde_json::json!({
        "cloudflare_pages_url": pages_url,
        "custom_domain": request.custom_domain,
    })))
}

async fn trigger_pages_deployment(
    state: &AppState,
    project_name: &str,
) -> Result<PagesDeploymentStatus, ApiError> {
    let url = format!(
        "https://api.cloudflare.com/client/v4/accounts/{}/pages/projects/{}/deployments",
        state.cf_account_id, project_name
    );
    let response = state
        .cf_client
        .post(url)
        .bearer_auth(&state.cf_api_token)
        .json(&serde_json::json!({}))
        .send()
        .await
        .map_err(|e| DomainError::Cloudflare(e.to_string()))?;
    let deployment = parse_pages_deployment_response(response).await?;
    Ok(deployment)
}

async fn wait_pages_deployment_success(
    state: &AppState,
    project_name: &str,
    deployment_id: &str,
    expected_commit_sha: Option<&str>,
) -> Result<PagesDeploymentStatus, ApiError> {
    for _ in 0..PAGES_DEPLOYMENT_POLL_ATTEMPTS {
        let url = format!(
            "https://api.cloudflare.com/client/v4/accounts/{}/pages/projects/{}/deployments/{}",
            state.cf_account_id, project_name, deployment_id
        );
        let response = state
            .cf_client
            .get(url)
            .bearer_auth(&state.cf_api_token)
            .send()
            .await
            .map_err(|e| DomainError::Cloudflare(e.to_string()))?;
        let deployment = parse_pages_deployment_response(response).await?;
        match deployment.stage_status.as_deref() {
            Some("success") => {
                verify_deployment_commit(&deployment, expected_commit_sha)?;
                return Ok(deployment);
            }
            Some("failure") | Some("canceled") => {
                return Err(DomainError::Cloudflare(format!(
                    "deployment {} ended with status {}",
                    deployment.id,
                    deployment.stage_status.unwrap_or_default()
                )).into());
            }
            _ => sleep(PAGES_DEPLOYMENT_POLL_INTERVAL).await,
        }
    }

    Err(DomainError::Cloudflare(format!(
        "deployment {deployment_id} did not complete before timeout"
    )).into())
}

async fn bind_pages_custom_domain(
    state: &AppState,
    project_name: &str,
    domain: &str,
) -> Result<(), ApiError> {
    if let Some(status) = get_pages_domain_status(state, project_name, domain).await? {
        ensure_domain_status_not_failed(domain, &status)?;
        return Ok(());
    }
    let bind_url = format!(
        "https://api.cloudflare.com/client/v4/accounts/{}/pages/projects/{}/domains",
        state.cf_account_id, project_name
    );
    let bind_response = state
        .cf_client
        .post(&bind_url)
        .bearer_auth(&state.cf_api_token)
        .json(&serde_json::json!({"name": domain}))
        .send()
        .await
        .map_err(|e| DomainError::Cloudflare(e.to_string()))?;

    let bind_status = bind_response.status();
    let bind_body = bind_response
        .text()
        .await
        .map_err(|e| DomainError::Cloudflare(e.to_string()))?;

    if !bind_status.is_success()
        || response_success(&bind_body)
            .map(|success| !success)
            .unwrap_or(false)
    {
        warn!(status = %bind_status, body = %bind_body, "custom domain bind failed");
        return Err(DomainError::Cloudflare(format!(
            "custom domain bind failed: {bind_status}: {bind_body}"
        )))?;
    }

    for _ in 0..PAGES_DOMAIN_POLL_ATTEMPTS {
        if let Some(status) = get_pages_domain_status(state, project_name, domain).await? {
            ensure_domain_status_not_failed(domain, &status)?;
        return Ok(());
        }
        sleep(PAGES_DOMAIN_POLL_INTERVAL).await;
    }

    Err(DomainError::Cloudflare(format!(
        "custom domain {domain} was not visible after binding"
    )))?
}

async fn get_pages_domain_status(
    state: &AppState,
    project_name: &str,
    domain: &str,
) -> Result<Option<String>, ApiError> {
    let url = format!(
        "https://api.cloudflare.com/client/v4/accounts/{}/pages/projects/{}/domains/{}",
        state.cf_account_id, project_name, domain
    );
    let response = state
        .cf_client
        .get(url)
        .bearer_auth(&state.cf_api_token)
        .send()
        .await
        .map_err(|e| DomainError::Cloudflare(e.to_string()))?;
    if response.status() == StatusCode::NOT_FOUND {
        return Ok(None);
    }
    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|e| DomainError::Cloudflare(e.to_string()))?;
    if !status.is_success()
        || response_success(&body)
            .map(|success| !success)
            .unwrap_or(false)
    {
        return Err(DomainError::Cloudflare(format!("{status}: {body}")))?;
    }
    let value: serde_json::Value = serde_json::from_str(&body)
        .map_err(|e| DomainError::Cloudflare(format!("parse domain response: {e}: {body}")))?;
    let domain_status = value
        .get("result")
        .and_then(|result| result.get("status"))
        .and_then(|status| status.as_str())
        .filter(|status| !status.trim().is_empty())
        .ok_or_else(|| DomainError::Cloudflare("domain response missing status".to_string()))?;
    Ok(Some(domain_status.to_string()))
}




// ---- ApiError ----

struct ApiError(DomainError);

impl From<DeploymentError> for ApiError {
    fn from(error: DeploymentError) -> Self {
        ApiError(DomainError::from(error))
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, error_code) = match &self.0 {
            DomainError::UnsupportedTld => (StatusCode::BAD_REQUEST, "unsupported_tld"),
            DomainError::Unavailable => (StatusCode::BAD_REQUEST, "unavailable"),
            DomainError::AlreadyRegisteredToAccount => {
                (StatusCode::CONFLICT, "already_registered_to_account")
            }
            DomainError::PremiumRejected => (StatusCode::BAD_REQUEST, "premium_rejected"),
            DomainError::MissingConfig(_) => (StatusCode::INTERNAL_SERVER_ERROR, "missing_config"),
            DomainError::Http(_) => (StatusCode::BAD_GATEWAY, "upstream_http_error"),
            DomainError::Parse(_) => (StatusCode::BAD_GATEWAY, "upstream_parse_error"),
            DomainError::Namecheap(_) => (StatusCode::BAD_GATEWAY, "namecheap_error"),
            DomainError::Cloudflare(_) => (StatusCode::BAD_GATEWAY, "cloudflare_error"),
        };
        let body = ApiErrorResponse {
            error: error_code.to_string(),
            message: self.0.to_string(),
        };
        (status, Json(body)).into_response()
    }
}

impl From<DomainError> for ApiError {
    fn from(error: DomainError) -> Self {
        Self(error)
    }
}

fn api_error_body(error: &str, message: &str) -> Json<ApiErrorResponse> {
    Json(ApiErrorResponse {
        error: error.to_string(),
        message: message.to_string(),
    })
}

// ---- Config from env ----

fn namecheap_config_from_env() -> NamecheapConfig {
    let first_name = env::var("NAMECHEAP_CONTACT_FIRST_NAME").unwrap_or_default();
    let last_name = env::var("NAMECHEAP_CONTACT_LAST_NAME").unwrap_or_default();
    let address1 = env::var("NAMECHEAP_CONTACT_ADDRESS1").unwrap_or_default();
    let city = env::var("NAMECHEAP_CONTACT_CITY").unwrap_or_default();
    let state_province = env::var("NAMECHEAP_CONTACT_STATE").unwrap_or_default();
    let postal_code = env::var("NAMECHEAP_CONTACT_POSTAL_CODE").unwrap_or_default();
    let country = env::var("NAMECHEAP_CONTACT_COUNTRY").unwrap_or_default();
    let phone = env::var("NAMECHEAP_CONTACT_PHONE").unwrap_or_default();
    let email = env::var("NAMECHEAP_CONTACT_EMAIL").unwrap_or_default();
    let organization = env::var("NAMECHEAP_CONTACT_ORGANIZATION")
        .ok()
        .filter(|v| !v.trim().is_empty());

    NamecheapConfig {
        api_user: env::var("NAMECHEAP_API_USER").unwrap_or_default(),
        api_key: env::var("NAMECHEAP_API_KEY").unwrap_or_default(),
        username: env::var("NAMECHEAP_USERNAME").unwrap_or_default(),
        client_ip: env::var("NAMECHEAP_CLIENT_IP").unwrap_or_default(),
        sandbox: env::var("NAMECHEAP_SANDBOX")
            .unwrap_or_else(|_| "true".to_string())
            .eq_ignore_ascii_case("true"),
        auto_purchase_after_tx: false,
        contact: NamecheapContactConfig {
            first_name,
            last_name,
            address1,
            city,
            state_province,
            postal_code,
            country,
            phone,
            email,
            organization,
        },
    }
}
