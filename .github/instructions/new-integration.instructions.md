---
description: "Use when adding a new external integration, payment chain, API client, or service boundary. Covers the trait-based boundary pattern used in this project."
---
# Adding External Integrations

Every external service call must go through a trait:

1. **Define a trait** with `#[async_trait]` in the relevant module
2. **Implement the real version** that calls the external API
3. **Implement a stub/dry-run** for tests (no real credentials needed)
4. **Use `anyhow::Result`** for the trait methods; define a `thiserror` enum if the caller needs to match on specific error variants
5. **Wire it up** at the consuming binary entry point via `Arc<dyn Trait>`

Existing examples:
- `PaymentVerifier` in [src/payment.rs](../../src/payment.rs)
- `SiteGenerationProvider` in [src/generation.rs](../../src/generation.rs)
- `SiteDeployer` in [src/deployment/mod.rs](../../src/deployment/mod.rs)
- `DomainRegistrar` / `DnsProvisioner` in [src/domain.rs](../../src/domain.rs)

When adding a new config value, update `Config` in [src/config.rs](../../src/config.rs), read it via `env::var(...)` in `Config::from_env()`, and document it in the matching `.env.*.example`.
