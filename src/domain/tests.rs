use std::str::FromStr;

use async_trait::async_trait;
use rust_decimal::Decimal;

use super::namecheap::icann_fee;
use super::namecheap_xml::NamecheapXml;
use super::*;
use crate::config::{NamecheapConfig, NamecheapContactConfig};

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
    assert_eq!(quote.available, true);
    assert_eq!(quote.is_premium, false);
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
    assert_eq!(quote.available, false);
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
    assert_eq!(quote.available, true);
    assert_eq!(quote.is_premium, true);
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
    let attrs = NamecheapXml::new(xml)
        .first_tag_attrs("DomainCheckResult")
        .unwrap();
    assert_eq!(attrs.bool("Available").unwrap(), true);
    assert_eq!(attrs.bool("IsPremiumName").unwrap(), false);
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
        NamecheapXml::new(xml).price_for_years("xyz", 1).unwrap(),
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
        NamecheapXml::new(xml).price_for_years("com", 2).unwrap(),
        Decimal::from_str("27.98").unwrap()
    );
}

#[test]
fn price_missing_tld_errors() {
    let xml = r#"<ApiResponse><CommandResponse><UserGetPricingResult><ProductType Name="DOMAIN"><ProductCategory Name="REGISTER"><Product Name="xyz"><Price Duration="1" DurationType="YEAR" Price="5.00" /></Product></ProductCategory></ProductType></UserGetPricingResult></CommandResponse></ApiResponse>"#;
    assert!(NamecheapXml::new(xml).price_for_years("com", 1).is_err());
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
    assert!(NamecheapXml::new(xml).price_for_years("xyz", 3).is_err());
}

#[test]
fn parses_domain_create_result() {
    let xml = r#"<ApiResponse Status="OK"><CommandResponse><DomainCreateResult Domain="pepe.xyz" Registered="true" ChargedAmount="12.3400" OrderID="123" TransactionID="456" /></CommandResponse></ApiResponse>"#;
    let attrs = NamecheapXml::new(xml)
        .first_tag_attrs("DomainCreateResult")
        .unwrap();
    assert_eq!(attrs.bool("Registered").unwrap(), true);
    assert_eq!(attrs.get("OrderID").unwrap(), "123");
    assert_eq!(attrs.get("TransactionID").unwrap(), "456");
    assert_eq!(
        attrs.decimal("ChargedAmount").unwrap(),
        Decimal::from_str("12.3400").unwrap()
    );
}

#[test]
fn parses_not_registered_result() {
    let xml = r#"<ApiResponse Status="OK"><CommandResponse><DomainCreateResult Domain="pepe.xyz" Registered="false" /></CommandResponse></ApiResponse>"#;
    let attrs = NamecheapXml::new(xml)
        .first_tag_attrs("DomainCreateResult")
        .unwrap();
    assert_eq!(attrs.bool("Registered").unwrap(), false);
}

#[test]
fn parses_premium_domain_check() {
    let xml = r#"<ApiResponse Status="OK"><CommandResponse><DomainCheckResult Domain="fancy.com" Available="true" IsPremiumName="true" PremiumRegistrationPrice="2500.00" EapFee="0.0" /></CommandResponse></ApiResponse>"#;
    let attrs = NamecheapXml::new(xml)
        .first_tag_attrs("DomainCheckResult")
        .unwrap();
    assert_eq!(attrs.bool("Available").unwrap(), true);
    assert_eq!(attrs.bool("IsPremiumName").unwrap(), true);
    assert_eq!(
        attrs.decimal("PremiumRegistrationPrice").unwrap(),
        Decimal::from_str("2500.00").unwrap()
    );
}

#[test]
fn detects_namecheap_error_in_response() {
    let xml = r#"<ApiResponse Status="ERROR"><CommandResponse><Error>Domain not available</Error></CommandResponse></ApiResponse>"#;
    assert_eq!(
        NamecheapXml::new(xml).first_tag_text("Error").unwrap(),
        "Domain not available"
    );
}

#[test]
fn parses_domain_get_info_owner_result() {
    let xml = r#"<ApiResponse Status="OK"><CommandResponse><DomainGetInfoResult DomainName="pepe.xyz" IsOwner="true" /></CommandResponse></ApiResponse>"#;
    let attrs = NamecheapXml::new(xml)
        .first_tag_attrs("DomainGetInfoResult")
        .unwrap();
    assert_eq!(attrs.get("DomainName").unwrap(), "pepe.xyz");
    assert_eq!(attrs.get("IsOwner").unwrap(), "true");
}

#[test]
fn attr_bool_accepts_truthy_values() {
    assert_eq!(
        NamecheapXml::parse_attrs(r#"<Tag Test="yes" />"#)
            .bool("Test")
            .unwrap(),
        true
    );
    assert_eq!(
        NamecheapXml::parse_attrs(r#"<Tag Test="1" />"#)
            .bool("Test")
            .unwrap(),
        true
    );
    assert_eq!(
        NamecheapXml::parse_attrs(r#"<Tag Test="TRUE" />"#)
            .bool("Test")
            .unwrap(),
        true
    );
}

#[test]
fn attr_bool_missing_key_errors() {
    let attrs = NamecheapXml::parse_attrs("<Tag />");
    assert!(attrs.bool("Missing").is_err());
}

#[test]
fn attr_decimal_missing_key_errors() {
    let attrs = NamecheapXml::parse_attrs("<Tag />");
    assert!(attrs.decimal("Missing").is_err());
}

#[test]
fn parses_attrs_with_multiple_fields() {
    let tag = r#"<DomainCheckResult Domain="pepe.xyz" Available="true" IsPremiumName="false" EapFee="0.0" />"#;
    let attrs = NamecheapXml::parse_attrs(tag);
    assert_eq!(attrs.get("Domain").unwrap(), "pepe.xyz");
    assert_eq!(attrs.get("Available").unwrap(), "true");
    assert_eq!(attrs.get("IsPremiumName").unwrap(), "false");
    assert_eq!(attrs.get("EapFee").unwrap(), "0.0");
}

#[test]
fn xml_unescape_decodes_entities() {
    assert_eq!(NamecheapXml::unescape("&amp;"), "&");
    assert_eq!(NamecheapXml::unescape("&lt;b&gt;"), "<b>");
    assert_eq!(NamecheapXml::unescape("&quot;hello&quot;"), "\"hello\"");
    assert_eq!(NamecheapXml::unescape("it&apos;s"), "it's");
}

// ---- XML parsing for DNS responses ----

#[test]
fn parses_dns_get_hosts_empty() {
    let xml = r#"<ApiResponse Status="OK"><CommandResponse><DomainDNSGetHostsResult Domain="pepe.xyz" EmailType="FWD" IsUsingOurDNS="true"><host /></DomainDNSGetHostsResult></CommandResponse></ApiResponse>"#;
    let attrs = NamecheapXml::new(xml)
        .first_tag_attrs("DomainDNSGetHostsResult")
        .unwrap();
    assert_eq!(attrs.get("Domain").unwrap(), "pepe.xyz");
}

#[test]
fn parses_dns_set_custom_result() {
    let xml = r#"<ApiResponse Status="OK"><CommandResponse><DomainDNSSetCustomResult Domain="pepe.xyz" Updated="true" /></CommandResponse></ApiResponse>"#;
    let attrs = NamecheapXml::new(xml)
        .first_tag_attrs("DomainDNSSetCustomResult")
        .unwrap();
    assert_eq!(attrs.bool("Updated").unwrap(), true);
}

#[test]
fn rejects_dns_set_custom_not_updated() {
    let xml = r#"<ApiResponse Status="OK"><CommandResponse><DomainDNSSetCustomResult Domain="pepe.xyz" Updated="false" /></CommandResponse></ApiResponse>"#;
    let attrs = NamecheapXml::new(xml)
        .first_tag_attrs("DomainDNSSetCustomResult")
        .unwrap();
    assert_eq!(attrs.bool("Updated").unwrap(), false);
}

#[test]
fn splits_namecheap_domain_parts_for_dns_api() {
    let (sld, tld) = NamecheapXml::domain_parts("Pepe.XYZ.").unwrap();
    assert_eq!(sld, "pepe");
    assert_eq!(tld, "xyz");
}

#[test]
fn icann_fee_is_20_cents() {
    assert_eq!(icann_fee(), Decimal::new(20, 2));
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
    if api_user.is_empty() || api_key.is_empty() || username.is_empty() || client_ip.is_empty() {
        return None;
    }
    Some(NamecheapConfig {
        api_user,
        api_key,
        username,
        client_ip,
        sandbox: true,
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
async fn real_sandbox_quote_multiple_tlds() {
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
async fn real_sandbox_missing_api_config() {
    let config = NamecheapConfig {
        api_user: String::new(),
        api_key: String::new(),
        username: String::new(),
        client_ip: String::new(),
        sandbox: true,
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

// ---- Real Cloudflare API tests ----
//
// Run with: cargo test domain -- --ignored
// Requires CLOUDFLARE_ACCOUNT_ID and CLOUDFLARE_API_TOKEN in .env.
