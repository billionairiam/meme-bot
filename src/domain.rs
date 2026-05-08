use std::{collections::HashMap, str::FromStr, time::Duration};

use async_trait::async_trait;
use reqwest::Client;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::config::{NamecheapConfig, NamecheapContactConfig};

const NAMECHEAP_SANDBOX_URL: &str = "https://api.sandbox.namecheap.com/xml.response";
const NAMECHEAP_PRODUCTION_URL: &str = "https://api.namecheap.com/xml.response";

pub const SUPPORTED_TLDS: &[&str] = &[
    "com", "net", "org", "xyz", "fun", "site", "online", "io", "co", "me", "cc", "app", "dev",
    "world", "space", "club", "store", "shop",
];

fn icann_fee(tld: &str) -> Decimal {
    match tld {
        "com" | "net" | "org" => Decimal::new(20, 2),
        _ => Decimal::ZERO,
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DomainQuoteRequest {
    pub domain: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DomainQuote {
    pub domain: String,
    pub tld: String,
    pub available: bool,
    pub is_premium: bool,
    pub price_usd: Decimal,
    pub years: i32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DomainPurchaseRequest {
    pub domain: String,
    pub years: i32,
    pub privacy_enabled: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DomainPurchaseResult {
    pub namecheap_order_id: String,
    pub namecheap_transaction_id: String,
    pub charged_amount_usd: Decimal,
}

#[derive(Debug, Error)]
pub enum DomainError {
    #[error("unsupported tld")]
    UnsupportedTld,
    #[error("domain unavailable")]
    Unavailable,
    #[error("premium or EAP domain requires manual handling")]
    PremiumRejected,
    #[error("missing Namecheap configuration: {0}")]
    MissingConfig(String),
    #[error("namecheap http error: {0}")]
    Http(String),
    #[error("namecheap parse error: {0}")]
    Parse(String),
    #[error("namecheap error: {0}")]
    Namecheap(String),
    #[error("cloudflare dns error: {0}")]
    Cloudflare(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiErrorResponse {
    pub error: String,
    pub message: String,
}

#[async_trait]
pub trait DomainRegistrar: Send + Sync {
    async fn quote(&self, request: &DomainQuoteRequest) -> Result<DomainQuote, DomainError>;

    async fn purchase(
        &self,
        request: &DomainPurchaseRequest,
    ) -> Result<DomainPurchaseResult, DomainError>;
}

pub struct NamecheapRegistrar {
    config: NamecheapConfig,
    client: Client,
    allowed_tlds: Vec<String>,
}

impl NamecheapRegistrar {
    pub fn new(config: NamecheapConfig) -> Self {
        Self {
            config,
            client: Client::new(),
            allowed_tlds: vec![
                "com".to_string(),
                "net".to_string(),
                "org".to_string(),
                "xyz".to_string(),
                "fun".to_string(),
                "site".to_string(),
                "online".to_string(),
                "io".to_string(),
                "co".to_string(),
                "me".to_string(),
                "cc".to_string(),
                "app".to_string(),
                "dev".to_string(),
                "world".to_string(),
                "space".to_string(),
                "club".to_string(),
                "store".to_string(),
                "shop".to_string(),
            ],
        }
    }

    fn endpoint(&self) -> &'static str {
        if self.config.sandbox {
            NAMECHEAP_SANDBOX_URL
        } else {
            NAMECHEAP_PRODUCTION_URL
        }
    }

    fn tld_for(domain: &str) -> Option<String> {
        domain
            .rsplit_once('.')
            .map(|(_, tld)| tld.to_ascii_lowercase())
    }

    fn validate_tld(&self, domain: &str) -> Result<String, DomainError> {
        let tld = Self::tld_for(domain).ok_or(DomainError::UnsupportedTld)?;
        if self.allowed_tlds.contains(&tld) {
            Ok(tld)
        } else {
            Err(DomainError::UnsupportedTld)
        }
    }

    fn validate_api_config(&self) -> Result<(), DomainError> {
        let missing = [
            ("NAMECHEAP_API_USER", &self.config.api_user),
            ("NAMECHEAP_API_KEY", &self.config.api_key),
            ("NAMECHEAP_USERNAME", &self.config.username),
            ("NAMECHEAP_CLIENT_IP", &self.config.client_ip),
        ]
        .iter()
        .filter_map(|(name, value)| value.trim().is_empty().then_some(*name))
        .collect::<Vec<_>>();

        if missing.is_empty() {
            Ok(())
        } else {
            Err(DomainError::MissingConfig(missing.join(", ")))
        }
    }

    fn validate_contact_config(&self) -> Result<(), DomainError> {
        let contact = &self.config.contact;
        let missing = [
            ("NAMECHEAP_CONTACT_FIRST_NAME", &contact.first_name),
            ("NAMECHEAP_CONTACT_LAST_NAME", &contact.last_name),
            ("NAMECHEAP_CONTACT_ADDRESS1", &contact.address1),
            ("NAMECHEAP_CONTACT_CITY", &contact.city),
            ("NAMECHEAP_CONTACT_STATE", &contact.state_province),
            ("NAMECHEAP_CONTACT_POSTAL_CODE", &contact.postal_code),
            ("NAMECHEAP_CONTACT_COUNTRY", &contact.country),
            ("NAMECHEAP_CONTACT_PHONE", &contact.phone),
            ("NAMECHEAP_CONTACT_EMAIL", &contact.email),
        ]
        .iter()
        .filter_map(|(name, value)| value.trim().is_empty().then_some(*name))
        .collect::<Vec<_>>();

        if missing.is_empty() {
            Ok(())
        } else {
            Err(DomainError::MissingConfig(missing.join(", ")))
        }
    }

    async fn call_namecheap(
        &self,
        command: &str,
        extra_params: Vec<(String, String)>,
    ) -> Result<String, DomainError> {
        self.validate_api_config()?;

        let mut params = vec![
            ("ApiUser".to_string(), self.config.api_user.clone()),
            ("ApiKey".to_string(), self.config.api_key.clone()),
            ("UserName".to_string(), self.config.username.clone()),
            ("ClientIp".to_string(), self.config.client_ip.clone()),
            ("Command".to_string(), command.to_string()),
        ];
        params.extend(extra_params);

        let response = self
            .client
            .post(self.endpoint())
            .form(&params)
            .send()
            .await
            .map_err(|error| DomainError::Http(error.to_string()))?;

        let status = response.status();
        let body = response
            .text()
            .await
            .map_err(|error| DomainError::Http(error.to_string()))?;

        if !status.is_success() {
            return Err(DomainError::Http(format!("{status}: {body}")));
        }

        if let Some(error) = first_tag_text(&body, "Error") {
            return Err(DomainError::Namecheap(error));
        }

        Ok(body)
    }

    async fn check_domain(&self, domain: &str) -> Result<NamecheapCheckResult, DomainError> {
        let xml = self
            .call_namecheap(
                "namecheap.domains.check",
                vec![("DomainList".to_string(), domain.to_string())],
            )
            .await?;
        let attrs = first_tag_attrs(&xml, "DomainCheckResult")
            .ok_or_else(|| DomainError::Parse("missing DomainCheckResult".to_string()))?;

        Ok(NamecheapCheckResult {
            available: attr_bool(&attrs, "Available")?,
            is_premium: attr_bool(&attrs, "IsPremiumName").unwrap_or(false),
            premium_registration_price: attr_decimal(&attrs, "PremiumRegistrationPrice").ok(),
            eap_fee: attr_decimal(&attrs, "EapFee").ok(),
        })
    }

    async fn registration_price(&self, tld: &str, years: i32) -> Result<Decimal, DomainError> {
        let xml = self
            .call_namecheap(
                "namecheap.users.getPricing",
                vec![
                    ("ProductType".to_string(), "DOMAIN".to_string()),
                    ("ProductCategory".to_string(), "DOMAINS".to_string()),
                    ("ActionName".to_string(), "REGISTER".to_string()),
                    ("ProductName".to_string(), tld.to_ascii_uppercase()),
                ],
            )
            .await?;

        parse_price_for_years(&xml, tld, years)
    }

    fn create_contact_params(&self) -> Vec<(String, String)> {
        let contact = &self.config.contact;
        let mut params = Vec::new();
        append_contact_params(&mut params, "Registrant", contact);
        append_contact_params(&mut params, "Admin", contact);
        append_contact_params(&mut params, "Tech", contact);
        append_contact_params(&mut params, "AuxBilling", contact);
        params
    }
}

#[async_trait]
impl DomainRegistrar for NamecheapRegistrar {
    async fn quote(&self, request: &DomainQuoteRequest) -> Result<DomainQuote, DomainError> {
        let domain = request.domain.to_ascii_lowercase();
        let tld = self.validate_tld(&domain)?;
        let check = self.check_domain(&domain).await?;

        if !check.available {
            return Ok(DomainQuote {
                domain,
                tld,
                available: false,
                is_premium: check.is_premium,
                price_usd: Decimal::ZERO,
                years: 1,
            });
        }

        if check.is_premium || check.eap_fee.unwrap_or(Decimal::ZERO) > Decimal::ZERO {
            return Ok(DomainQuote {
                domain,
                tld,
                available: true,
                is_premium: true,
                price_usd: check.premium_registration_price.unwrap_or(Decimal::ZERO),
                years: 1,
            });
        }

        let price_usd = self.registration_price(&tld, 1).await? + icann_fee(&tld);
        Ok(DomainQuote {
            domain,
            tld,
            available: true,
            is_premium: false,
            price_usd,
            years: 1,
        })
    }

    async fn purchase(
        &self,
        request: &DomainPurchaseRequest,
    ) -> Result<DomainPurchaseResult, DomainError> {
        let domain = request.domain.to_ascii_lowercase();
        self.validate_tld(&domain)?;
        self.validate_contact_config()?;

        let check = self.check_domain(&domain).await?;
        if !check.available {
            return Err(DomainError::Unavailable);
        }
        if check.is_premium || check.eap_fee.unwrap_or(Decimal::ZERO) > Decimal::ZERO {
            return Err(DomainError::PremiumRejected);
        }

        let mut params = vec![
            ("DomainName".to_string(), domain.clone()),
            ("Years".to_string(), request.years.max(1).to_string()),
        ];
        if request.privacy_enabled {
            params.push(("AddFreeWhoisguard".to_string(), "yes".to_string()));
            params.push(("WGEnabled".to_string(), "yes".to_string()));
        }
        params.extend(self.create_contact_params());

        let xml = self
            .call_namecheap("namecheap.domains.create", params)
            .await?;
        let attrs = first_tag_attrs(&xml, "DomainCreateResult")
            .ok_or_else(|| DomainError::Parse("missing DomainCreateResult".to_string()))?;

        if !attr_bool(&attrs, "Registered").unwrap_or(false) {
            return Err(DomainError::Namecheap(format!(
                "domain was not registered: {xml}"
            )));
        }

        Ok(DomainPurchaseResult {
            namecheap_order_id: required_attr(&attrs, "OrderID")?,
            namecheap_transaction_id: required_attr(&attrs, "TransactionID")?,
            charged_amount_usd: attr_decimal(&attrs, "ChargedAmount")?,
        })
    }
}

// ---- HttpClientDomainRegistrar ----

pub struct HttpClientDomainRegistrar {
    base_url: String,
    api_key: String,
    client: Client,
}

impl HttpClientDomainRegistrar {
    pub fn new(base_url: String, api_key: String) -> Self {
        Self {
            base_url,
            api_key,
            client: Client::new(),
        }
    }
}

async fn map_http_error(response: reqwest::Response) -> DomainError {
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    match serde_json::from_str::<ApiErrorResponse>(&body) {
        Ok(api_err) => match api_err.error.as_str() {
            "unsupported_tld" => DomainError::UnsupportedTld,
            "unavailable" => DomainError::Unavailable,
            "premium_rejected" => DomainError::PremiumRejected,
            "missing_config" => DomainError::MissingConfig(api_err.message),
            "namecheap_error" => DomainError::Namecheap(api_err.message),
            "cloudflare_error" => DomainError::Cloudflare(api_err.message),
            "upstream_http_error" | "upstream_parse_error" => DomainError::Http(api_err.message),
            _ => DomainError::Http(format!("{status}: {}", api_err.message)),
        },
        Err(_) => DomainError::Http(format!("{status}: {body}")),
    }
}

#[async_trait]
impl DomainRegistrar for HttpClientDomainRegistrar {
    async fn quote(&self, request: &DomainQuoteRequest) -> Result<DomainQuote, DomainError> {
        let response = self
            .client
            .post(format!("{}/api/v1/domain/quote", self.base_url))
            .header("X-API-Key", &self.api_key)
            .json(request)
            .timeout(Duration::from_secs(30))
            .send()
            .await
            .map_err(|e| DomainError::Http(e.to_string()))?;

        if response.status().is_success() {
            response
                .json()
                .await
                .map_err(|e| DomainError::Parse(e.to_string()))
        } else {
            Err(map_http_error(response).await)
        }
    }

    async fn purchase(
        &self,
        request: &DomainPurchaseRequest,
    ) -> Result<DomainPurchaseResult, DomainError> {
        let response = self
            .client
            .post(format!("{}/api/v1/domain/purchase", self.base_url))
            .header("X-API-Key", &self.api_key)
            .json(request)
            .timeout(Duration::from_secs(30))
            .send()
            .await
            .map_err(|e| DomainError::Http(e.to_string()))?;

        if response.status().is_success() {
            response
                .json()
                .await
                .map_err(|e| DomainError::Parse(e.to_string()))
        } else {
            Err(map_http_error(response).await)
        }
    }
}

#[derive(Debug)]
struct NamecheapCheckResult {
    available: bool,
    is_premium: bool,
    premium_registration_price: Option<Decimal>,
    eap_fee: Option<Decimal>,
}

fn append_contact_params(
    params: &mut Vec<(String, String)>,
    prefix: &str,
    contact: &NamecheapContactConfig,
) {
    params.push((
        contact_param_name(prefix, "FirstName"),
        contact.first_name.clone(),
    ));
    params.push((
        contact_param_name(prefix, "LastName"),
        contact.last_name.clone(),
    ));
    params.push((
        contact_param_name(prefix, "Address1"),
        contact.address1.clone(),
    ));
    params.push((contact_param_name(prefix, "City"), contact.city.clone()));
    params.push((
        contact_param_name(prefix, "StateProvince"),
        contact.state_province.clone(),
    ));
    params.push((
        contact_param_name(prefix, "PostalCode"),
        contact.postal_code.clone(),
    ));
    params.push((
        contact_param_name(prefix, "Country"),
        contact.country.clone(),
    ));
    params.push((contact_param_name(prefix, "Phone"), contact.phone.clone()));
    params.push((
        contact_param_name(prefix, "EmailAddress"),
        contact.email.clone(),
    ));
    if let Some(organization) = &contact.organization {
        params.push((
            contact_param_name(prefix, "OrganizationName"),
            organization.clone(),
        ));
    }
}

fn contact_param_name(prefix: &str, field: &str) -> String {
    format!("{prefix}{field}")
}

fn parse_price_for_years(xml: &str, tld: &str, years: i32) -> Result<Decimal, DomainError> {
    let register_body = product_category_body(xml, "REGISTER")
        .ok_or_else(|| DomainError::Parse("missing REGISTER pricing category".to_string()))?;
    parse_price_for_years_in_category(register_body, tld, years)
}

fn product_category_body<'a>(xml: &'a str, category_name: &str) -> Option<&'a str> {
    let mut cursor = 0;
    while let Some(start) = xml[cursor..].find("<ProductCategory ") {
        let absolute_start = cursor + start;
        let Some(end) = xml[absolute_start..].find('>') else {
            break;
        };
        let category_tag = &xml[absolute_start..absolute_start + end + 1];
        let category_attrs = parse_attrs(category_tag);
        let name = category_attrs
            .get("Name")
            .map(|value| value.to_ascii_uppercase());

        let category_close = "</ProductCategory>";
        let content_start = absolute_start + end + 1;
        let Some(close_offset) = xml[content_start..].find(category_close) else {
            break;
        };
        let category_body = &xml[content_start..content_start + close_offset];

        if name.as_deref() == Some(category_name) {
            return Some(category_body);
        }
        cursor = content_start + close_offset + category_close.len();
    }

    None
}

fn parse_price_for_years_in_category(
    category_body: &str,
    tld: &str,
    years: i32,
) -> Result<Decimal, DomainError> {
    let mut cursor = 0;
    while let Some(start) = category_body[cursor..].find("<Product ") {
        let absolute_start = cursor + start;
        let Some(end) = category_body[absolute_start..].find('>') else {
            break;
        };
        let product_tag = &category_body[absolute_start..absolute_start + end + 1];
        let product_attrs = parse_attrs(product_tag);
        let product_name = product_attrs
            .get("Name")
            .map(|value| value.trim_start_matches('.').to_ascii_lowercase());

        let product_close = "</Product>";
        let content_start = absolute_start + end + 1;
        let Some(close_offset) = category_body[content_start..].find(product_close) else {
            break;
        };
        let product_body = &category_body[content_start..content_start + close_offset];

        if product_name.as_deref() == Some(tld) {
            return parse_price_in_product(product_body, years);
        }
        cursor = content_start + close_offset + product_close.len();
    }

    Err(DomainError::Parse(format!("missing pricing for .{tld}")))
}

fn parse_price_in_product(product_body: &str, years: i32) -> Result<Decimal, DomainError> {
    let mut cursor = 0;
    while let Some(start) = product_body[cursor..].find("<Price ") {
        let absolute_start = cursor + start;
        let Some(end) = product_body[absolute_start..].find('>') else {
            break;
        };
        let tag = &product_body[absolute_start..absolute_start + end + 1];
        let attrs = parse_attrs(tag);
        let duration = attrs
            .get("Duration")
            .and_then(|value| value.parse::<i32>().ok());
        let duration_type = attrs
            .get("DurationType")
            .map(|value| value.to_ascii_uppercase());

        if duration == Some(years) && duration_type.as_deref() == Some("YEAR") {
            return attr_decimal(&attrs, "Price");
        }
        cursor = absolute_start + end + 1;
    }

    Err(DomainError::Parse(format!(
        "missing {years} year registration price"
    )))
}

fn first_tag_text(xml: &str, tag: &str) -> Option<String> {
    let open_start = xml.find(&format!("<{tag}"))?;
    let open_end = xml[open_start..].find('>')? + open_start;
    let close_start = xml[open_end + 1..].find(&format!("</{tag}>"))? + open_end + 1;
    Some(xml[open_end + 1..close_start].trim().to_string())
}

fn first_tag_attrs(xml: &str, tag: &str) -> Option<HashMap<String, String>> {
    let start = xml.find(&format!("<{tag} "))?;
    let end = xml[start..].find('>')? + start;
    Some(parse_attrs(&xml[start..=end]))
}

fn parse_attrs(tag: &str) -> HashMap<String, String> {
    let mut attrs = HashMap::new();
    let bytes = tag.as_bytes();
    let mut index = 0;

    while index < bytes.len() {
        while index < bytes.len() && !bytes[index].is_ascii_alphabetic() {
            index += 1;
        }
        let key_start = index;
        while index < bytes.len()
            && (bytes[index].is_ascii_alphanumeric()
                || bytes[index] == b'_'
                || bytes[index] == b'-')
        {
            index += 1;
        }
        if key_start == index {
            break;
        }
        let key = &tag[key_start..index];

        while index < bytes.len() && bytes[index].is_ascii_whitespace() {
            index += 1;
        }
        if index >= bytes.len() || bytes[index] != b'=' {
            continue;
        }
        index += 1;
        while index < bytes.len() && bytes[index].is_ascii_whitespace() {
            index += 1;
        }
        if index >= bytes.len() || bytes[index] != b'"' {
            continue;
        }
        index += 1;
        let value_start = index;
        while index < bytes.len() && bytes[index] != b'"' {
            index += 1;
        }
        if index <= bytes.len() {
            attrs.insert(key.to_string(), xml_unescape(&tag[value_start..index]));
        }
        index += 1;
    }

    attrs
}

fn attr_bool(attrs: &HashMap<String, String>, name: &str) -> Result<bool, DomainError> {
    attrs
        .get(name)
        .map(|value| matches!(value.to_ascii_lowercase().as_str(), "true" | "yes" | "1"))
        .ok_or_else(|| DomainError::Parse(format!("missing {name}")))
}

fn attr_decimal(attrs: &HashMap<String, String>, name: &str) -> Result<Decimal, DomainError> {
    let value = attrs
        .get(name)
        .ok_or_else(|| DomainError::Parse(format!("missing {name}")))?;
    Decimal::from_str(value).map_err(|error| DomainError::Parse(error.to_string()))
}

fn required_attr(attrs: &HashMap<String, String>, name: &str) -> Result<String, DomainError> {
    attrs
        .get(name)
        .filter(|value| !value.trim().is_empty())
        .cloned()
        .ok_or_else(|| DomainError::Parse(format!("missing {name}")))
}

fn namecheap_domain_parts(domain: &str) -> Result<(String, String), DomainError> {
    let domain = domain.trim().trim_end_matches('.').to_ascii_lowercase();
    let (sld, tld) = domain.rsplit_once('.').ok_or(DomainError::UnsupportedTld)?;
    if sld.is_empty() || tld.is_empty() {
        return Err(DomainError::UnsupportedTld);
    }
    Ok((sld.to_string(), tld.to_string()))
}

fn xml_unescape(value: &str) -> String {
    value
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CloudflareZoneResult {
    pub zone_id: String,
    pub nameservers: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DnsRecord {
    pub host_name: String,
    pub record_type: DnsRecordType,
    pub address: String,
    pub ttl: Option<i32>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum DnsRecordType {
    A,
    Aaaa,
    Cname,
    Txt,
    Mx,
    MxE,
    Url,
    Url301,
    Frame,
}

impl DnsRecordType {
    fn as_namecheap_str(self) -> &'static str {
        match self {
            Self::A => "A",
            Self::Aaaa => "AAAA",
            Self::Cname => "CNAME",
            Self::Txt => "TXT",
            Self::Mx => "MX",
            Self::MxE => "MXE",
            Self::Url => "URL",
            Self::Url301 => "URL301",
            Self::Frame => "FRAME",
        }
    }
}

#[async_trait]
pub trait DnsProvisioner: Send + Sync {
    async fn create_zone(&self, domain: &str) -> Result<CloudflareZoneResult, DomainError>;

    async fn set_namecheap_nameservers(
        &self,
        domain: &str,
        nameservers: &[String],
    ) -> Result<(), DomainError>;

    async fn set_dns_records(&self, domain: &str, records: &[DnsRecord])
    -> Result<(), DomainError>;
}

// ---- DryRunDnsProvisioner ----

pub struct DryRunDnsProvisioner;

#[async_trait]
impl DnsProvisioner for DryRunDnsProvisioner {
    async fn create_zone(&self, domain: &str) -> Result<CloudflareZoneResult, DomainError> {
        Ok(CloudflareZoneResult {
            zone_id: format!("dry-run-zone-{domain}"),
            nameservers: vec![
                "ns1.cloudflare.com".to_string(),
                "ns2.cloudflare.com".to_string(),
            ],
        })
    }

    async fn set_namecheap_nameservers(
        &self,
        _domain: &str,
        _nameservers: &[String],
    ) -> Result<(), DomainError> {
        Ok(())
    }

    async fn set_dns_records(
        &self,
        _domain: &str,
        _records: &[DnsRecord],
    ) -> Result<(), DomainError> {
        Ok(())
    }
}

// ---- NamecheapDnsProvisioner (real API) ----

pub struct NamecheapDnsProvisioner {
    config: NamecheapConfig,
    client: Client,
}

impl NamecheapDnsProvisioner {
    pub fn new(config: NamecheapConfig) -> Self {
        Self {
            config,
            client: Client::new(),
        }
    }

    fn endpoint(&self) -> &'static str {
        if self.config.sandbox {
            NAMECHEAP_SANDBOX_URL
        } else {
            NAMECHEAP_PRODUCTION_URL
        }
    }

    fn validate_api_config(&self) -> Result<(), DomainError> {
        let missing = [
            ("NAMECHEAP_API_USER", &self.config.api_user),
            ("NAMECHEAP_API_KEY", &self.config.api_key),
            ("NAMECHEAP_USERNAME", &self.config.username),
            ("NAMECHEAP_CLIENT_IP", &self.config.client_ip),
        ]
        .iter()
        .filter_map(|(name, value)| value.trim().is_empty().then_some(*name))
        .collect::<Vec<_>>();

        if missing.is_empty() {
            Ok(())
        } else {
            Err(DomainError::MissingConfig(missing.join(", ")))
        }
    }

    async fn call_namecheap(
        &self,
        command: &str,
        extra_params: Vec<(String, String)>,
    ) -> Result<String, DomainError> {
        self.validate_api_config()?;

        let mut params = vec![
            ("ApiUser".to_string(), self.config.api_user.clone()),
            ("ApiKey".to_string(), self.config.api_key.clone()),
            ("UserName".to_string(), self.config.username.clone()),
            ("ClientIp".to_string(), self.config.client_ip.clone()),
            ("Command".to_string(), command.to_string()),
        ];
        params.extend(extra_params);

        let response = self
            .client
            .post(self.endpoint())
            .form(&params)
            .send()
            .await
            .map_err(|error| DomainError::Http(error.to_string()))?;

        let status = response.status();
        let body = response
            .text()
            .await
            .map_err(|error| DomainError::Http(error.to_string()))?;

        if !status.is_success() {
            return Err(DomainError::Http(format!("{status}: {body}")));
        }

        if let Some(error) = first_tag_text(&body, "Error") {
            return Err(DomainError::Namecheap(error));
        }

        Ok(body)
    }
}

#[async_trait]
impl DnsProvisioner for NamecheapDnsProvisioner {
    async fn create_zone(&self, _domain: &str) -> Result<CloudflareZoneResult, DomainError> {
        Err(DomainError::Cloudflare(
            "create_zone not supported by Namecheap; use Cloudflare API directly".to_string(),
        ))
    }

    async fn set_namecheap_nameservers(
        &self,
        domain: &str,
        nameservers: &[String],
    ) -> Result<(), DomainError> {
        let (sld, tld) = namecheap_domain_parts(domain)?;
        let ns_list = nameservers.join(",");
        let xml = self
            .call_namecheap(
                "namecheap.domains.dns.setCustom",
                vec![
                    ("SLD".to_string(), sld),
                    ("TLD".to_string(), tld),
                    ("Nameservers".to_string(), ns_list),
                ],
            )
            .await?;
        let attrs = first_tag_attrs(&xml, "DomainDNSSetCustomResult")
            .ok_or_else(|| DomainError::Parse("missing DomainDNSSetCustomResult".to_string()))?;
        if !attr_bool(&attrs, "Updated").unwrap_or(false) {
            return Err(DomainError::Namecheap(format!(
                "nameserver update was not applied: {xml}"
            )));
        }
        Ok(())
    }

    async fn set_dns_records(
        &self,
        domain: &str,
        records: &[DnsRecord],
    ) -> Result<(), DomainError> {
        let (sld, tld) = namecheap_domain_parts(domain)?;
        let mut params = vec![("SLD".to_string(), sld), ("TLD".to_string(), tld)];
        for (i, record) in records.iter().enumerate() {
            let n = i + 1;
            params.push((format!("HostName{n}"), record.host_name.clone()));
            params.push((
                format!("RecordType{n}"),
                record.record_type.as_namecheap_str().to_string(),
            ));
            params.push((format!("Address{n}"), record.address.clone()));
            if let Some(ttl) = record.ttl {
                params.push((format!("TTL{n}"), ttl.to_string()));
            }
        }
        let xml = self
            .call_namecheap("namecheap.domains.dns.setHosts", params)
            .await?;
        let attrs = first_tag_attrs(&xml, "DomainDNSSetHostsResult")
            .ok_or_else(|| DomainError::Parse("missing DomainDNSSetHostsResult".to_string()))?;
        if !attr_bool(&attrs, "IsSuccess").unwrap_or(false) {
            return Err(DomainError::Namecheap(format!(
                "dns host records were not applied: {xml}"
            )));
        }
        Ok(())
    }
}

// ---- CloudflareDnsProvisioner (real API) ----

pub struct CloudflareDnsProvisioner {
    account_id: String,
    api_token: String,
    client: Client,
}

impl CloudflareDnsProvisioner {
    pub fn new(account_id: String, api_token: String) -> Self {
        Self {
            account_id,
            api_token,
            client: Client::new(),
        }
    }

    fn validate_config(&self) -> Result<(), DomainError> {
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
            Err(DomainError::MissingConfig(missing.join(", ")))
        }
    }
}

#[async_trait]
impl DnsProvisioner for CloudflareDnsProvisioner {
    async fn create_zone(&self, domain: &str) -> Result<CloudflareZoneResult, DomainError> {
        self.validate_config()?;

        let url = "https://api.cloudflare.com/client/v4/zones";
        let response = self
            .client
            .post(url)
            .bearer_auth(&self.api_token)
            .json(&serde_json::json!({
                "name": domain,
                "account": { "id": self.account_id },
                "jump_start": true,
            }))
            .send()
            .await
            .map_err(|error| DomainError::Cloudflare(error.to_string()))?;

        let status = response.status();
        let body = response
            .text()
            .await
            .map_err(|error| DomainError::Cloudflare(error.to_string()))?;

        if !status.is_success() {
            return Err(DomainError::Cloudflare(format!("{status}: {body}")));
        }

        let json: serde_json::Value = serde_json::from_str(&body)
            .map_err(|error| DomainError::Cloudflare(format!("parse response: {error}")))?;

        if json.get("success").and_then(|v| v.as_bool()) != Some(true) {
            let errors = json
                .get("errors")
                .map(|e| e.to_string())
                .unwrap_or_else(|| "unknown error".to_string());
            return Err(DomainError::Cloudflare(format!(
                "zone creation failed: {errors}"
            )));
        }

        let result = json
            .get("result")
            .ok_or_else(|| DomainError::Cloudflare("missing result in response".to_string()))?;

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

    async fn set_namecheap_nameservers(
        &self,
        _domain: &str,
        _nameservers: &[String],
    ) -> Result<(), DomainError> {
        Err(DomainError::Cloudflare(
            "set_namecheap_nameservers not supported by Cloudflare; use NamecheapDnsProvisioner"
                .to_string(),
        ))
    }

    async fn set_dns_records(
        &self,
        _domain: &str,
        _records: &[DnsRecord],
    ) -> Result<(), DomainError> {
        Err(DomainError::Cloudflare(
            "set_dns_records not supported by Cloudflare; use NamecheapDnsProvisioner".to_string(),
        ))
    }
}

// ---- HttpClientDnsProvisioner ----

pub struct HttpClientDnsProvisioner {
    base_url: String,
    api_key: String,
    client: Client,
}

impl HttpClientDnsProvisioner {
    pub fn new(base_url: String, api_key: String) -> Self {
        Self {
            base_url,
            api_key,
            client: Client::new(),
        }
    }
}

#[async_trait]
impl DnsProvisioner for HttpClientDnsProvisioner {
    async fn create_zone(&self, domain: &str) -> Result<CloudflareZoneResult, DomainError> {
        let response = self
            .client
            .post(format!("{}/api/v1/dns/create-zone", self.base_url))
            .header("X-API-Key", &self.api_key)
            .json(&serde_json::json!({"domain": domain}))
            .timeout(Duration::from_secs(30))
            .send()
            .await
            .map_err(|e| DomainError::Http(e.to_string()))?;

        if response.status().is_success() {
            response
                .json()
                .await
                .map_err(|e| DomainError::Parse(e.to_string()))
        } else {
            Err(map_http_error(response).await)
        }
    }

    async fn set_namecheap_nameservers(
        &self,
        domain: &str,
        nameservers: &[String],
    ) -> Result<(), DomainError> {
        let response = self
            .client
            .post(format!("{}/api/v1/dns/set-nameservers", self.base_url))
            .header("X-API-Key", &self.api_key)
            .json(&serde_json::json!({"domain": domain, "nameservers": nameservers}))
            .timeout(Duration::from_secs(30))
            .send()
            .await
            .map_err(|e| DomainError::Http(e.to_string()))?;

        if response.status().is_success() {
            Ok(())
        } else {
            Err(map_http_error(response).await)
        }
    }

    async fn set_dns_records(
        &self,
        domain: &str,
        records: &[DnsRecord],
    ) -> Result<(), DomainError> {
        let response = self
            .client
            .post(format!("{}/api/v1/dns/set-records", self.base_url))
            .header("X-API-Key", &self.api_key)
            .json(&serde_json::json!({"domain": domain, "records": records}))
            .timeout(Duration::from_secs(30))
            .send()
            .await
            .map_err(|e| DomainError::Http(e.to_string()))?;

        if response.status().is_success() {
            Ok(())
        } else {
            Err(map_http_error(response).await)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- mock registrar for full trait contract tests ----

    struct MockDomainRegistrar {
        available: bool,
        is_premium: bool,
        price_usd: Decimal,
        purchase_fails: bool,
    }

    impl MockDomainRegistrar {
        fn available(price_usd: Decimal) -> Self {
            Self {
                available: true,
                is_premium: false,
                price_usd,
                purchase_fails: false,
            }
        }

        fn unavailable() -> Self {
            Self {
                available: false,
                is_premium: false,
                price_usd: Decimal::ZERO,
                purchase_fails: false,
            }
        }

        fn premium(price_usd: Decimal) -> Self {
            Self {
                available: true,
                is_premium: true,
                price_usd,
                purchase_fails: false,
            }
        }

        fn purchase_failing(price_usd: Decimal) -> Self {
            Self {
                available: true,
                is_premium: false,
                price_usd,
                purchase_fails: true,
            }
        }
    }

    #[async_trait]
    impl DomainRegistrar for MockDomainRegistrar {
        async fn quote(&self, request: &DomainQuoteRequest) -> Result<DomainQuote, DomainError> {
            let tld = request
                .domain
                .rsplit_once('.')
                .map(|(_, tld)| tld.to_ascii_lowercase())
                .ok_or(DomainError::UnsupportedTld)?;
            Ok(DomainQuote {
                domain: request.domain.clone(),
                tld,
                available: self.available,
                is_premium: self.is_premium,
                price_usd: self.price_usd,
                years: 1,
            })
        }

        async fn purchase(
            &self,
            _request: &DomainPurchaseRequest,
        ) -> Result<DomainPurchaseResult, DomainError> {
            if self.purchase_fails {
                return Err(DomainError::Namecheap("purchase failed".to_string()));
            }
            if !self.available {
                return Err(DomainError::Unavailable);
            }
            if self.is_premium {
                return Err(DomainError::PremiumRejected);
            }
            Ok(DomainPurchaseResult {
                namecheap_order_id: "mock-order-1".to_string(),
                namecheap_transaction_id: "mock-txn-1".to_string(),
                charged_amount_usd: self.price_usd,
            })
        }
    }

    // ---- end-to-end flow tests ----

    #[tokio::test]
    async fn full_quote_purchase_flow_available_domain() {
        let registrar = MockDomainRegistrar::available(Decimal::from_str("9.99").unwrap());
        let quote = registrar
            .quote(&DomainQuoteRequest {
                domain: "mytoken.xyz".to_string(),
            })
            .await
            .unwrap();
        assert!(quote.available);
        assert!(!quote.is_premium);
        assert_eq!(quote.price_usd, Decimal::from_str("9.99").unwrap());
        assert_eq!(quote.tld, "xyz");

        let purchase = registrar
            .purchase(&DomainPurchaseRequest {
                domain: "mytoken.xyz".to_string(),
                years: 1,
                privacy_enabled: true,
            })
            .await
            .unwrap();
        assert_eq!(purchase.namecheap_order_id, "mock-order-1");
        assert_eq!(purchase.namecheap_transaction_id, "mock-txn-1");
    }

    #[tokio::test]
    async fn quote_unavailable_domain() {
        let registrar = MockDomainRegistrar::unavailable();
        let quote = registrar
            .quote(&DomainQuoteRequest {
                domain: "taken.xyz".to_string(),
            })
            .await
            .unwrap();
        assert!(!quote.available);
        assert_eq!(quote.price_usd, Decimal::ZERO);
    }

    #[tokio::test]
    async fn quote_premium_domain_rejected() {
        let registrar = MockDomainRegistrar::premium(Decimal::from_str("2500.00").unwrap());
        let quote = registrar
            .quote(&DomainQuoteRequest {
                domain: "premium.xyz".to_string(),
            })
            .await
            .unwrap();
        assert!(quote.available);
        assert!(quote.is_premium);
        assert_eq!(quote.price_usd, Decimal::from_str("2500.00").unwrap());
    }

    #[tokio::test]
    async fn purchase_unavailable_domain_errors() {
        let registrar = MockDomainRegistrar::unavailable();
        let err = registrar
            .purchase(&DomainPurchaseRequest {
                domain: "taken.xyz".to_string(),
                years: 1,
                privacy_enabled: true,
            })
            .await
            .unwrap_err();
        assert!(matches!(err, DomainError::Unavailable));
    }

    #[tokio::test]
    async fn purchase_premium_domain_errors() {
        let registrar = MockDomainRegistrar::premium(Decimal::from_str("2500.00").unwrap());
        let err = registrar
            .purchase(&DomainPurchaseRequest {
                domain: "premium.xyz".to_string(),
                years: 1,
                privacy_enabled: true,
            })
            .await
            .unwrap_err();
        assert!(matches!(err, DomainError::PremiumRejected));
    }

    #[tokio::test]
    async fn purchase_failure_propagates_error() {
        let registrar = MockDomainRegistrar::purchase_failing(Decimal::from_str("9.99").unwrap());
        let err = registrar
            .purchase(&DomainPurchaseRequest {
                domain: "ok.xyz".to_string(),
                years: 1,
                privacy_enabled: true,
            })
            .await
            .unwrap_err();
        assert!(matches!(err, DomainError::Namecheap(_)));
    }

    // ---- XML parsing tests ----

    #[test]
    fn parses_domain_check_attrs() {
        let xml = r#"<ApiResponse Status="OK"><CommandResponse><DomainCheckResult Domain="pepe.xyz" Available="true" IsPremiumName="false" EapFee="0.0" /></CommandResponse></ApiResponse>"#;
        let attrs = first_tag_attrs(xml, "DomainCheckResult").unwrap();
        assert!(attr_bool(&attrs, "Available").unwrap());
        assert!(!attr_bool(&attrs, "IsPremiumName").unwrap());
    }

    #[test]
    fn parses_one_year_price() {
        let xml = r#"
        <ApiResponse Status="OK">
          <CommandResponse>
            <UserGetPricingResult>
              <ProductType Name="DOMAIN">
                <ProductCategory Name="REGISTER">
                  <Product Name="xyz">
                    <Price Duration="1" DurationType="YEAR" Price="12.34" />
                    <Price Duration="2" DurationType="YEAR" Price="24.68" />
                  </Product>
                </ProductCategory>
              </ProductType>
            </UserGetPricingResult>
          </CommandResponse>
        </ApiResponse>
        "#;
        assert_eq!(
            parse_price_for_years(xml, "xyz", 1).unwrap(),
            Decimal::from_str("12.34").unwrap()
        );
    }

    #[test]
    fn parses_two_year_price() {
        let xml = r#"
        <ApiResponse Status="OK">
          <CommandResponse>
            <UserGetPricingResult>
              <ProductType Name="DOMAIN">
                <ProductCategory Name="REGISTER">
                  <Product Name="com">
                    <Price Duration="1" DurationType="YEAR" Price="13.99" />
                    <Price Duration="2" DurationType="YEAR" Price="27.98" />
                  </Product>
                </ProductCategory>
              </ProductType>
            </UserGetPricingResult>
          </CommandResponse>
        </ApiResponse>
        "#;
        assert_eq!(
            parse_price_for_years(xml, "com", 2).unwrap(),
            Decimal::from_str("27.98").unwrap()
        );
    }

    #[test]
    fn price_missing_tld_errors() {
        let xml = r#"<ApiResponse><CommandResponse><UserGetPricingResult><ProductType Name="DOMAIN"><ProductCategory Name="REGISTER"><Product Name="xyz"><Price Duration="1" DurationType="YEAR" Price="5.00" /></Product></ProductCategory></ProductType></UserGetPricingResult></CommandResponse></ApiResponse>"#;
        assert!(parse_price_for_years(xml, "com", 1).is_err());
    }

    #[test]
    fn price_missing_duration_errors() {
        let xml = r#"
        <ApiResponse>
          <CommandResponse>
            <UserGetPricingResult>
              <ProductType Name="DOMAIN">
                <ProductCategory Name="REGISTER">
                  <Product Name="xyz">
                    <Price Duration="1" DurationType="YEAR" Price="5.00" />
                  </Product>
                </ProductCategory>
              </ProductType>
            </UserGetPricingResult>
          </CommandResponse>
        </ApiResponse>
        "#;
        assert!(parse_price_for_years(xml, "xyz", 3).is_err());
    }

    #[test]
    fn parses_domain_create_result() {
        let xml = r#"<ApiResponse Status="OK"><CommandResponse><DomainCreateResult Domain="pepe.xyz" Registered="true" ChargedAmount="12.3400" OrderID="123" TransactionID="456" /></CommandResponse></ApiResponse>"#;
        let attrs = first_tag_attrs(xml, "DomainCreateResult").unwrap();
        assert!(attr_bool(&attrs, "Registered").unwrap());
        assert_eq!(attrs.get("OrderID").unwrap(), "123");
        assert_eq!(attrs.get("TransactionID").unwrap(), "456");
        assert_eq!(
            attr_decimal(&attrs, "ChargedAmount").unwrap(),
            Decimal::from_str("12.3400").unwrap()
        );
    }

    #[test]
    fn parses_not_registered_result() {
        let xml = r#"<ApiResponse Status="OK"><CommandResponse><DomainCreateResult Domain="pepe.xyz" Registered="false" /></CommandResponse></ApiResponse>"#;
        let attrs = first_tag_attrs(xml, "DomainCreateResult").unwrap();
        assert!(!attr_bool(&attrs, "Registered").unwrap());
    }

    #[test]
    fn parses_premium_domain_check() {
        let xml = r#"<ApiResponse Status="OK"><CommandResponse><DomainCheckResult Domain="fancy.com" Available="true" IsPremiumName="true" PremiumRegistrationPrice="2500.00" EapFee="0.0" /></CommandResponse></ApiResponse>"#;
        let attrs = first_tag_attrs(xml, "DomainCheckResult").unwrap();
        assert!(attr_bool(&attrs, "Available").unwrap());
        assert!(attr_bool(&attrs, "IsPremiumName").unwrap());
        assert_eq!(
            attr_decimal(&attrs, "PremiumRegistrationPrice").unwrap(),
            Decimal::from_str("2500.00").unwrap()
        );
    }

    #[test]
    fn detects_namecheap_error_in_response() {
        let xml = r#"<ApiResponse Status="ERROR"><CommandResponse><Error>Domain not available</Error></CommandResponse></ApiResponse>"#;
        assert_eq!(
            first_tag_text(xml, "Error").unwrap(),
            "Domain not available"
        );
    }

    #[test]
    fn attr_bool_accepts_truthy_values() {
        let mut attrs = HashMap::new();
        attrs.insert("Test".to_string(), "yes".to_string());
        assert!(attr_bool(&attrs, "Test").unwrap());
        attrs.insert("Test".to_string(), "1".to_string());
        assert!(attr_bool(&attrs, "Test").unwrap());
        attrs.insert("Test".to_string(), "TRUE".to_string());
        assert!(attr_bool(&attrs, "Test").unwrap());
    }

    #[test]
    fn attr_bool_missing_key_errors() {
        let attrs: HashMap<String, String> = HashMap::new();
        assert!(attr_bool(&attrs, "Missing").is_err());
    }

    #[test]
    fn attr_decimal_missing_key_errors() {
        let attrs: HashMap<String, String> = HashMap::new();
        assert!(attr_decimal(&attrs, "Missing").is_err());
    }

    #[test]
    fn parses_attrs_with_multiple_fields() {
        let tag = r#"<DomainCheckResult Domain="pepe.xyz" Available="true" IsPremiumName="false" EapFee="0.0" />"#;
        let attrs = parse_attrs(tag);
        assert_eq!(attrs.get("Domain").unwrap(), "pepe.xyz");
        assert_eq!(attrs.get("Available").unwrap(), "true");
        assert_eq!(attrs.get("IsPremiumName").unwrap(), "false");
        assert_eq!(attrs.get("EapFee").unwrap(), "0.0");
    }

    #[test]
    fn xml_unescape_decodes_entities() {
        assert_eq!(xml_unescape("&amp;"), "&");
        assert_eq!(xml_unescape("&lt;b&gt;"), "<b>");
        assert_eq!(xml_unescape("&quot;hello&quot;"), "\"hello\"");
        assert_eq!(xml_unescape("it&apos;s"), "it's");
    }

    // ---- DryRunDnsProvisioner tests ----

    #[tokio::test]
    async fn dry_run_creates_zone() {
        let provisioner = DryRunDnsProvisioner;
        let zone = provisioner.create_zone("pepe.xyz").await.unwrap();
        assert!(zone.zone_id.contains("pepe.xyz"));
        assert_eq!(zone.nameservers.len(), 2);
        assert!(
            zone.nameservers
                .iter()
                .all(|ns| ns.ends_with(".cloudflare.com"))
        );
    }

    #[tokio::test]
    async fn dry_run_set_nameservers_succeeds() {
        let provisioner = DryRunDnsProvisioner;
        let result = provisioner
            .set_namecheap_nameservers(
                "pepe.xyz",
                &[
                    "ns1.cloudflare.com".to_string(),
                    "ns2.cloudflare.com".to_string(),
                ],
            )
            .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn dry_run_set_dns_records_succeeds() {
        let provisioner = DryRunDnsProvisioner;
        let result = provisioner
            .set_dns_records(
                "pepe.xyz",
                &[
                    DnsRecord {
                        host_name: "@".to_string(),
                        record_type: DnsRecordType::A,
                        address: "192.0.2.1".to_string(),
                        ttl: Some(1800),
                    },
                    DnsRecord {
                        host_name: "www".to_string(),
                        record_type: DnsRecordType::Cname,
                        address: "pepe.xyz".to_string(),
                        ttl: None,
                    },
                ],
            )
            .await;
        assert!(result.is_ok());
    }

    // ---- DnsRecordType tests ----

    #[test]
    fn dns_record_type_namecheap_str() {
        assert_eq!(DnsRecordType::A.as_namecheap_str(), "A");
        assert_eq!(DnsRecordType::Aaaa.as_namecheap_str(), "AAAA");
        assert_eq!(DnsRecordType::Cname.as_namecheap_str(), "CNAME");
        assert_eq!(DnsRecordType::Txt.as_namecheap_str(), "TXT");
        assert_eq!(DnsRecordType::Mx.as_namecheap_str(), "MX");
        assert_eq!(DnsRecordType::MxE.as_namecheap_str(), "MXE");
        assert_eq!(DnsRecordType::Url.as_namecheap_str(), "URL");
        assert_eq!(DnsRecordType::Url301.as_namecheap_str(), "URL301");
        assert_eq!(DnsRecordType::Frame.as_namecheap_str(), "FRAME");
    }

    // ---- XML parsing for DNS responses ----

    #[test]
    fn parses_dns_get_hosts_empty() {
        let xml = r#"<ApiResponse Status="OK"><CommandResponse><DomainDNSGetHostsResult Domain="pepe.xyz" EmailType="FWD" IsUsingOurDNS="true"><host /></DomainDNSGetHostsResult></CommandResponse></ApiResponse>"#;
        let attrs = first_tag_attrs(xml, "DomainDNSGetHostsResult").unwrap();
        assert_eq!(attrs.get("Domain").unwrap(), "pepe.xyz");
    }

    #[test]
    fn parses_dns_set_custom_result() {
        let xml = r#"<ApiResponse Status="OK"><CommandResponse><DomainDNSSetCustomResult Domain="pepe.xyz" Updated="true" /></CommandResponse></ApiResponse>"#;
        let attrs = first_tag_attrs(xml, "DomainDNSSetCustomResult").unwrap();
        assert!(attr_bool(&attrs, "Updated").unwrap());
    }

    #[test]
    fn rejects_dns_set_custom_not_updated() {
        let xml = r#"<ApiResponse Status="OK"><CommandResponse><DomainDNSSetCustomResult Domain="pepe.xyz" Updated="false" /></CommandResponse></ApiResponse>"#;
        let attrs = first_tag_attrs(xml, "DomainDNSSetCustomResult").unwrap();
        assert!(!attr_bool(&attrs, "Updated").unwrap());
    }

    #[test]
    fn parses_dns_set_hosts_result() {
        let xml = r#"<ApiResponse Status="OK"><CommandResponse><DomainDNSSetHostsResult Domain="pepe.xyz" IsSuccess="true" /></CommandResponse></ApiResponse>"#;
        let attrs = first_tag_attrs(xml, "DomainDNSSetHostsResult").unwrap();
        assert!(attr_bool(&attrs, "IsSuccess").unwrap());
    }

    #[test]
    fn splits_namecheap_domain_parts_for_dns_api() {
        let (sld, tld) = namecheap_domain_parts("Pepe.XYZ.").unwrap();
        assert_eq!(sld, "pepe");
        assert_eq!(tld, "xyz");
    }

    #[test]
    fn adds_icann_fee_for_com_net_org() {
        assert_eq!(icann_fee("com"), Decimal::new(20, 2));
        assert_eq!(icann_fee("net"), Decimal::new(20, 2));
        assert_eq!(icann_fee("org"), Decimal::new(20, 2));
    }

    #[test]
    fn no_icann_fee_for_other_tlds() {
        for tld in ["xyz", "fun", "site", "online", "io", "co", "me", "cc", "app", "dev"] {
            assert_eq!(icann_fee(tld), Decimal::ZERO);
        }
    }

    // ---- Real Namecheap sandbox API tests ----
    //
    // These call the real Namecheap sandbox API. Run with:
    //   cargo test domain -- --ignored
    //
    // Prerequisites:
    //   1. Fill in NAMECHEAP_API_USER, NAMECHEAP_API_KEY, NAMECHEAP_USERNAME,
    //      NAMECHEAP_CLIENT_IP in .env (same keys as .env.example).
    //   2. Your server IP must be whitelisted in Namecheap (Settings → API).
    //   3. NAMECHEAP_SANDBOX=true (default in .env.example).
    //
    // For purchase tests, also fill all NAMECHEAP_CONTACT_* fields.

    fn sandbox_config() -> Option<NamecheapConfig> {
        let api_user = std::env::var("NAMECHEAP_API_USER").ok()?;
        let api_key = std::env::var("NAMECHEAP_API_KEY").ok()?;
        let username = std::env::var("NAMECHEAP_USERNAME").ok()?;
        let client_ip = std::env::var("NAMECHEAP_CLIENT_IP").ok()?;
        if api_user.is_empty() || api_key.is_empty() || username.is_empty() || client_ip.is_empty()
        {
            return None;
        }
        Some(NamecheapConfig {
            api_user,
            api_key,
            username,
            client_ip,
            sandbox: true,
            auto_purchase_after_tx: false,
            contact: sandbox_contact_config(),
        })
    }

    fn sandbox_contact_config() -> NamecheapContactConfig {
        NamecheapContactConfig {
            first_name: std::env::var("NAMECHEAP_CONTACT_FIRST_NAME").unwrap_or_default(),
            last_name: std::env::var("NAMECHEAP_CONTACT_LAST_NAME").unwrap_or_default(),
            address1: std::env::var("NAMECHEAP_CONTACT_ADDRESS1").unwrap_or_default(),
            city: std::env::var("NAMECHEAP_CONTACT_CITY").unwrap_or_default(),
            state_province: std::env::var("NAMECHEAP_CONTACT_STATE").unwrap_or_default(),
            postal_code: std::env::var("NAMECHEAP_CONTACT_POSTAL_CODE").unwrap_or_default(),
            country: std::env::var("NAMECHEAP_CONTACT_COUNTRY").unwrap_or_default(),
            phone: std::env::var("NAMECHEAP_CONTACT_PHONE").unwrap_or_default(),
            email: std::env::var("NAMECHEAP_CONTACT_EMAIL").unwrap_or_default(),
            organization: std::env::var("NAMECHEAP_CONTACT_ORGANIZATION")
                .ok()
                .filter(|v| !v.trim().is_empty()),
        }
    }

    #[tokio::test]
    #[ignore]
    async fn real_sandbox_quote_available_domain() {
        let config = sandbox_config().expect("Namecheap API credentials not set in env");
        let registrar = NamecheapRegistrar::new(config);
        let quote = registrar
            .quote(&DomainQuoteRequest {
                domain: "test-meme-bot-2026.xyz".to_string(),
            })
            .await
            .expect("quote call failed");
        assert!(quote.available, "expected domain to be available");
        assert!(!quote.is_premium, "expected non-premium domain");
        assert!(quote.price_usd > Decimal::ZERO, "expected positive price");
        assert_eq!(quote.tld, "xyz");
    }

    #[tokio::test]
    #[ignore]
    async fn real_sandbox_quote_dot_com_pricing() {
        let config = sandbox_config().expect("Namecheap API credentials not set in env");
        let registrar = NamecheapRegistrar::new(config);
        let quote = registrar
            .quote(&DomainQuoteRequest {
                domain: "test-meme-bot-2026.com".to_string(),
            })
            .await
            .expect("quote call failed");
        assert!(quote.available);
        assert!(quote.price_usd > Decimal::ZERO);
        assert_eq!(quote.tld, "com");
    }

    #[tokio::test]
    #[ignore]
    async fn real_sandbox_quote_supported_tlds() {
        let config = sandbox_config().expect("Namecheap API credentials not set in env");
        let registrar = NamecheapRegistrar::new(config);
        for (domain, expected_tld) in [
            ("test-meme.xyz", "xyz"),
            ("test-meme.site", "site"),
            ("test-meme.online", "online"),
            ("test-meme.fun", "fun"),
        ] {
            let quote = registrar
                .quote(&DomainQuoteRequest {
                    domain: domain.to_string(),
                })
                .await
                .expect("quote call failed");
            assert!(quote.available, "expected {domain} available");
            assert_eq!(quote.tld, expected_tld);
            assert!(quote.price_usd > Decimal::ZERO);
        }
    }

    #[tokio::test]
    #[ignore]
    async fn real_sandbox_unsupported_tld_rejected() {
        let config = sandbox_config().expect("Namecheap API credentials not set in env");
        let registrar = NamecheapRegistrar::new(config);
        let err = registrar
            .quote(&DomainQuoteRequest {
                domain: "test.page".to_string(),
            })
            .await
            .unwrap_err();
        assert!(matches!(err, DomainError::UnsupportedTld));
    }

    #[tokio::test]
    #[ignore]
    async fn real_sandbox_missing_api_config() {
        let config = NamecheapConfig {
            api_user: String::new(),
            api_key: String::new(),
            username: String::new(),
            client_ip: String::new(),
            sandbox: true,
            auto_purchase_after_tx: false,
            contact: sandbox_contact_config(),
        };
        let registrar = NamecheapRegistrar::new(config);
        let err = registrar
            .quote(&DomainQuoteRequest {
                domain: "test.xyz".to_string(),
            })
            .await
            .unwrap_err();
        assert!(matches!(err, DomainError::MissingConfig(_)));
    }

    #[tokio::test]
    #[ignore]
    async fn real_sandbox_purchase_domain() {
        let config = sandbox_config().expect("Namecheap API credentials not set in env");
        // purchase also needs contact config
        let missing_contact = [
            config.contact.first_name.is_empty(),
            config.contact.last_name.is_empty(),
            config.contact.address1.is_empty(),
            config.contact.city.is_empty(),
            config.contact.state_province.is_empty(),
            config.contact.postal_code.is_empty(),
            config.contact.country.is_empty(),
            config.contact.phone.is_empty(),
            config.contact.email.is_empty(),
        ];
        if missing_contact.iter().any(|m| *m) {
            eprintln!("skipping purchase test: NAMECHEAP_CONTACT_* fields not set");
            return;
        }

        let registrar = NamecheapRegistrar::new(config);
        let result = registrar
            .purchase(&DomainPurchaseRequest {
                domain: "test-meme-bot-sandbox.xyz".to_string(),
                years: 1,
                privacy_enabled: true,
            })
            .await;
        match result {
            Ok(purchase) => {
                assert!(!purchase.namecheap_order_id.is_empty());
                assert!(!purchase.namecheap_transaction_id.is_empty());
            }
            Err(DomainError::Namecheap(ref msg)) if msg.contains("Validation") => {
                eprintln!("Namecheap sandbox validation: {msg}");
            }
            Err(e) => panic!("purchase failed: {e}"),
        }
    }

    #[tokio::test]
    #[ignore]
    async fn real_sandbox_purchase_missing_contact_rejected() {
        let mut config = sandbox_config().expect("Namecheap API credentials not set in env");
        // wipe contact fields
        config.contact = NamecheapContactConfig {
            first_name: String::new(),
            last_name: String::new(),
            address1: String::new(),
            city: String::new(),
            state_province: String::new(),
            postal_code: String::new(),
            country: String::new(),
            phone: String::new(),
            email: String::new(),
            organization: None,
        };
        let registrar = NamecheapRegistrar::new(config);
        let err = registrar
            .purchase(&DomainPurchaseRequest {
                domain: "test-meme.xyz".to_string(),
                years: 1,
                privacy_enabled: true,
            })
            .await
            .unwrap_err();
        assert!(matches!(err, DomainError::MissingConfig(_)));
    }

    // ---- Real Namecheap sandbox DNS API tests ----
    //
    // Run with: cargo test domain -- --ignored
    // Same prerequisites as the domain quote/purchase tests above.

    #[tokio::test]
    #[ignore]
    async fn real_sandbox_set_nameservers() {
        let config = sandbox_config().expect("Namecheap API credentials not set in env");
        let provisioner = NamecheapDnsProvisioner::new(config);
        let result = provisioner
            .set_namecheap_nameservers(
                "test-meme.xyz",
                &[
                    "ns1.cloudflare.com".to_string(),
                    "ns2.cloudflare.com".to_string(),
                ],
            )
            .await;
        match result {
            Ok(()) => {}
            Err(DomainError::Namecheap(ref msg)) if msg.contains("not found") => {
                eprintln!("Namecheap sandbox domain not registered: {msg}");
            }
            Err(e) => panic!("set_nameservers failed: {e}"),
        }
    }

    #[tokio::test]
    #[ignore]
    async fn real_sandbox_set_dns_records() {
        let config = sandbox_config().expect("Namecheap API credentials not set in env");
        let provisioner = NamecheapDnsProvisioner::new(config);
        let result = provisioner
            .set_dns_records(
                "test-meme.xyz",
                &[
                    DnsRecord {
                        host_name: "@".to_string(),
                        record_type: DnsRecordType::A,
                        address: "192.0.2.1".to_string(),
                        ttl: Some(1800),
                    },
                    DnsRecord {
                        host_name: "www".to_string(),
                        record_type: DnsRecordType::Cname,
                        address: "test-meme.xyz".to_string(),
                        ttl: None,
                    },
                ],
            )
            .await;
        match result {
            Ok(()) => {}
            Err(DomainError::Namecheap(ref msg)) if msg.contains("not found") => {
                eprintln!("Namecheap sandbox domain not registered: {msg}");
            }
            Err(e) => panic!("set_dns_records failed: {e}"),
        }
    }

    #[tokio::test]
    #[ignore]
    async fn real_sandbox_dns_create_zone_not_supported() {
        let config = sandbox_config().expect("Namecheap API credentials not set in env");
        let provisioner = NamecheapDnsProvisioner::new(config);
        let err = provisioner.create_zone("test.xyz").await.unwrap_err();
        assert!(matches!(err, DomainError::Cloudflare(_)));
    }

    // ---- Real Cloudflare API tests ----
    //
    // Run with: cargo test domain -- --ignored
    // Requires CLOUDFLARE_ACCOUNT_ID and CLOUDFLARE_API_TOKEN in .env.

    fn cloudflare_provisioner() -> Option<CloudflareDnsProvisioner> {
        let account_id = std::env::var("CLOUDFLARE_ACCOUNT_ID").ok()?;
        let api_token = std::env::var("CLOUDFLARE_API_TOKEN").ok()?;
        if account_id.is_empty() || api_token.is_empty() {
            return None;
        }
        Some(CloudflareDnsProvisioner::new(account_id, api_token))
    }

    #[tokio::test]
    #[ignore]
    async fn real_cloudflare_create_zone() {
        let provisioner = cloudflare_provisioner()
            .expect("CLOUDFLARE_ACCOUNT_ID and CLOUDFLARE_API_TOKEN not set");
        let result = provisioner.create_zone("test-meme-cloudflare.xyz").await;
        match result {
            Ok(zone) => {
                assert!(!zone.zone_id.is_empty());
                assert!(!zone.nameservers.is_empty());
                assert!(
                    zone.nameservers
                        .iter()
                        .any(|ns| ns.contains("cloudflare.com"))
                );
            }
            Err(DomainError::Cloudflare(ref msg)) if msg.contains("already exists") => {
                eprintln!("zone already exists on Cloudflare");
            }
            Err(e) => panic!("create_zone failed: {e}"),
        }
    }

    #[tokio::test]
    #[ignore]
    async fn real_cloudflare_missing_config() {
        let provisioner = CloudflareDnsProvisioner::new(String::new(), String::new());
        let err = provisioner.create_zone("test.xyz").await.unwrap_err();
        assert!(matches!(err, DomainError::MissingConfig(_)));
    }

    // ---- Full DNS flow test (Cloudflare zone → Namecheap nameservers) ----
    //
    // This test demonstrates the complete domain setup flow:
    //   1. Create Cloudflare zone → get assigned nameservers
    //   2. Update Namecheap domain's nameservers to point to Cloudflare
    //
    // Run with: cargo test domain -- --ignored
    // Requires both Cloudflare and Namecheap credentials.

    #[tokio::test]
    #[ignore]
    async fn real_full_dns_flow_cloudflare_zone_then_namecheap_nameservers() {
        let cf_provisioner = cloudflare_provisioner().expect("Cloudflare credentials not set");
        let nc_config = sandbox_config().expect("Namecheap API credentials not set in env");
        let nc_provisioner = NamecheapDnsProvisioner::new(nc_config);

        // Step 1: create zone on Cloudflare, get assigned nameservers
        let zone = cf_provisioner.create_zone("test-meme-fullflow.xyz").await;
        let nameservers = match zone {
            Ok(zone) => {
                assert!(!zone.zone_id.is_empty());
                assert!(!zone.nameservers.is_empty());
                zone.nameservers
            }
            Err(DomainError::Cloudflare(ref msg)) if msg.contains("already exists") => {
                eprintln!("zone already exists, using known nameservers");
                vec![
                    "lennon.ns.cloudflare.com".to_string(),
                    "zoe.ns.cloudflare.com".to_string(),
                ]
            }
            Err(e) => panic!("create_zone failed: {e}"),
        };

        // Step 2: update Namecheap nameservers to Cloudflare
        let result = nc_provisioner
            .set_namecheap_nameservers("test-meme-fullflow.xyz", &nameservers)
            .await;
        match result {
            Ok(()) => {}
            Err(DomainError::Namecheap(ref msg)) if msg.contains("not found") => {
                eprintln!("domain not registered on Namecheap sandbox; skipping nameserver update");
            }
            Err(e) => panic!("set_nameservers failed: {e}"),
        }
    }
}
