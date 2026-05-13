use async_trait::async_trait;
use reqwest::Client;
use rust_decimal::Decimal;

use super::namecheap_xml::NamecheapXml;
use super::{
    DomainError, DomainPurchaseRequest, DomainPurchaseResult, DomainQuote, DomainQuoteRequest,
    DomainRegistrar,
};
use crate::config::{NamecheapConfig, NamecheapContactConfig};

const NAMECHEAP_SANDBOX_URL: &str = "https://api.sandbox.namecheap.com/xml.response";
const NAMECHEAP_PRODUCTION_URL: &str = "https://api.namecheap.com/xml.response";

pub(super) fn icann_fee() -> Decimal {
    Decimal::new(20, 2)
}

// ---- Shared Namecheap API transport ----

pub(super) struct NamecheapApiClient {
    config: NamecheapConfig,
    client: Client,
}

impl NamecheapApiClient {
    pub(super) fn new(config: NamecheapConfig) -> Self {
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

    pub(super) async fn call(
        &self,
        command: &str,
        extra_params: &[(String, String)],
    ) -> Result<String, DomainError> {
        self.validate_api_config()?;

        let mut params = vec![
            ("ApiUser".to_string(), self.config.api_user.clone()),
            ("ApiKey".to_string(), self.config.api_key.clone()),
            ("UserName".to_string(), self.config.username.clone()),
            ("ClientIp".to_string(), self.config.client_ip.clone()),
            ("Command".to_string(), command.to_string()),
        ];
        params.extend_from_slice(extra_params);

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

        if let Some(error) = NamecheapXml::new(&body).first_tag_text("Error") {
            return Err(DomainError::Namecheap(error));
        }

        Ok(body)
    }
}

// ---- NamecheapRegistrar ----

pub struct NamecheapRegistrar {
    api: NamecheapApiClient,
}

impl NamecheapRegistrar {
    pub fn new(config: NamecheapConfig) -> Self {
        Self {
            api: NamecheapApiClient::new(config),
        }
    }

    fn tld_for(domain: &str) -> Result<String, DomainError> {
        domain
            .rsplit_once('.')
            .map(|(_, tld)| tld.to_ascii_lowercase())
            .filter(|tld| !tld.is_empty())
            .ok_or(DomainError::UnsupportedTld)
    }

    fn validate_contact_config(&self) -> Result<(), DomainError> {
        let contact = &self.api.config.contact;
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

    async fn check_domain(&self, domain: &str) -> Result<NamecheapCheckResult, DomainError> {
        let xml = self
            .api
            .call(
                "namecheap.domains.check",
                &[("DomainList".to_string(), domain.to_string())],
            )
            .await?;
        let attrs = NamecheapXml::new(&xml)
            .first_tag_attrs("DomainCheckResult")
            .ok_or_else(|| DomainError::Parse("missing DomainCheckResult".to_string()))?;

        Ok(NamecheapCheckResult {
            available: attrs.bool("Available")?,
            is_premium: attrs.bool("IsPremiumName").unwrap_or(false),
            premium_registration_price: attrs.decimal("PremiumRegistrationPrice").ok(),
            eap_fee: attrs.decimal("EapFee").ok(),
        })
    }

    async fn account_owns_domain(&self, domain: &str) -> Result<bool, DomainError> {
        match self
            .api
            .call(
                "namecheap.domains.getInfo",
                &[("DomainName".to_string(), domain.to_string())],
            )
            .await
        {
            Ok(xml) => {
                let Some(attrs) = NamecheapXml::new(&xml).first_tag_attrs("DomainGetInfoResult")
                else {
                    return Ok(false);
                };
                let result_domain = attrs
                    .get("DomainName")
                    .or_else(|| attrs.get("Domain"))
                    .map(|value| value.eq_ignore_ascii_case(domain))
                    .unwrap_or(true);
                let is_owner = attrs
                    .get("IsOwner")
                    .map(|value| {
                        matches!(value.to_ascii_lowercase().as_str(), "true" | "yes" | "1")
                    })
                    .unwrap_or(true);
                Ok(result_domain && is_owner)
            }
            Err(DomainError::Namecheap(_)) => Ok(false),
            Err(error) => Err(error),
        }
    }

    async fn registration_price(&self, tld: &str, years: i32) -> Result<Decimal, DomainError> {
        let xml = self
            .api
            .call(
                "namecheap.users.getPricing",
                &[
                    ("ProductType".to_string(), "DOMAIN".to_string()),
                    ("ProductCategory".to_string(), "DOMAINS".to_string()),
                    ("ActionName".to_string(), "REGISTER".to_string()),
                    ("ProductName".to_string(), tld.to_ascii_uppercase()),
                ],
            )
            .await?;

        NamecheapXml::new(&xml).price_for_years(tld, years)
    }

    fn create_contact_params(&self) -> Vec<(String, String)> {
        let contact = &self.api.config.contact;
        let mut params = Vec::new();
        for prefix in ["Registrant", "Admin", "Tech", "AuxBilling"] {
            NamecheapContactParams { prefix, contact }.append_to(&mut params);
        }
        params
    }
}

#[async_trait]
impl DomainRegistrar for NamecheapRegistrar {
    async fn quote(&self, request: &DomainQuoteRequest) -> Result<DomainQuote, DomainError> {
        let domain = request.domain.to_ascii_lowercase();
        let tld = Self::tld_for(&domain)?;
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

        let price_usd = self.registration_price(&tld, 1).await? + icann_fee();
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
        Self::tld_for(&domain)?;
        self.validate_contact_config()?;

        let check = self.check_domain(&domain).await?;
        if !check.available {
            if self.account_owns_domain(&domain).await? {
                return Err(DomainError::AlreadyRegisteredToAccount);
            }
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

        let xml = self.api.call("namecheap.domains.create", &params).await?;
        let attrs = NamecheapXml::new(&xml)
            .first_tag_attrs("DomainCreateResult")
            .ok_or_else(|| DomainError::Parse("missing DomainCreateResult".to_string()))?;

        if !attrs.bool("Registered").unwrap_or(false) {
            return Err(DomainError::Namecheap(format!(
                "domain was not registered: {xml}"
            )));
        }

        Ok(DomainPurchaseResult {
            namecheap_order_id: attrs.required("OrderID")?,
            namecheap_transaction_id: attrs.required("TransactionID")?,
            charged_amount_usd: attrs.decimal("ChargedAmount")?,
        })
    }
}

// HttpClientDomainRegistrar and HttpClientDnsProvisioner live in api_client module

#[derive(Debug)]
struct NamecheapCheckResult {
    available: bool,
    is_premium: bool,
    premium_registration_price: Option<Decimal>,
    eap_fee: Option<Decimal>,
}

struct NamecheapContactParams<'a> {
    prefix: &'a str,
    contact: &'a NamecheapContactConfig,
}

impl NamecheapContactParams<'_> {
    fn append_to(&self, params: &mut Vec<(String, String)>) {
        self.push(params, "FirstName", &self.contact.first_name);
        self.push(params, "LastName", &self.contact.last_name);
        self.push(params, "Address1", &self.contact.address1);
        self.push(params, "City", &self.contact.city);
        self.push(params, "StateProvince", &self.contact.state_province);
        self.push(params, "PostalCode", &self.contact.postal_code);
        self.push(params, "Country", &self.contact.country);
        self.push(params, "Phone", &self.contact.phone);
        self.push(params, "EmailAddress", &self.contact.email);
        if let Some(organization) = &self.contact.organization {
            self.push(params, "OrganizationName", organization);
        }
    }

    fn push(&self, params: &mut Vec<(String, String)>, field: &str, value: &str) {
        params.push((self.param_name(field), value.to_string()));
    }

    fn param_name(&self, field: &str) -> String {
        format!("{}{field}", self.prefix)
    }
}
