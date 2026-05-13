use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use super::DomainError;
use super::namecheap::NamecheapApiClient;
use super::namecheap_xml::NamecheapXml;
use crate::config::NamecheapConfig;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CloudflareZoneResult {
    pub zone_id: String,
    pub nameservers: Vec<String>,
}

#[async_trait]
pub trait DnsProvisioner: Send + Sync {
    async fn set_namecheap_nameservers(
        &self,
        domain: &str,
        nameservers: &[String],
    ) -> Result<(), DomainError>;
}

pub struct NamecheapDnsProvisioner {
    api: NamecheapApiClient,
}

impl NamecheapDnsProvisioner {
    pub fn new(config: NamecheapConfig) -> Self {
        Self {
            api: NamecheapApiClient::new(config),
        }
    }

    pub async fn set_namecheap_nameservers(
        &self,
        domain: &str,
        nameservers: &[String],
    ) -> Result<(), DomainError> {
        let (sld, tld) = NamecheapXml::domain_parts(domain)?;
        let ns_list = nameservers.join(",");
        let xml = self
            .api
            .call(
                "namecheap.domains.dns.setCustom",
                &[
                    ("SLD".to_string(), sld),
                    ("TLD".to_string(), tld),
                    ("Nameservers".to_string(), ns_list),
                ],
            )
            .await?;
        let attrs = NamecheapXml::new(&xml)
            .first_tag_attrs("DomainDNSSetCustomResult")
            .ok_or_else(|| DomainError::Parse("missing DomainDNSSetCustomResult".to_string()))?;
        if !attrs.bool("Updated").unwrap_or(false) {
            return Err(DomainError::Namecheap(format!(
                "nameserver update was not applied: {xml}"
            )));
        }
        Ok(())
    }
}

#[async_trait]
impl DnsProvisioner for NamecheapDnsProvisioner {
    async fn set_namecheap_nameservers(
        &self,
        domain: &str,
        nameservers: &[String],
    ) -> Result<(), DomainError> {
        self.set_namecheap_nameservers(domain, nameservers).await
    }
}
