mod dns;
mod namecheap;
mod namecheap_xml;
mod registrar;

pub use dns::{CloudflareZoneResult, DnsProvisioner, NamecheapDnsProvisioner};
pub use namecheap::NamecheapRegistrar;
pub use registrar::{
    ApiErrorResponse, DomainError, DomainPurchaseRequest, DomainPurchaseResult, DomainQuote,
    DomainQuoteRequest, DomainRegistrar, SUGGESTED_TLDS,
};

#[cfg(test)]
mod tests;
