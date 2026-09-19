# Copilot instructions

## Build, run, test, and lint

This is a Rust 2024 crate with two binaries and a shared library.

```bash
cargo build                        # debug build
cargo run                          # runs meme-bot (default binary)
cargo run --bin api-server         # runs the REST API server
cargo test                         # run all tests
cargo test <test_name>             # run a single test, e.g. cargo test renders_wrangler_args
cargo test -- --nocapture          # show test output
cargo fmt --check                  # check formatting
cargo fmt                          # format code
cargo clippy                       # lint
cargo build-musl-release           # static Linux release build alias
```

For local setup, copy `.env.meme-bot.example` to `.env.meme-bot` and `.env.api-server.example` to `.env.api-server`. `meme-bot` loads `.env.meme-bot` by default, `api-server` loads `.env.api-server`; override with `MEME_BOT_ENV_FILE` or `API_SERVER_ENV_FILE`. Use `RUST_LOG=debug` or `trace` for verbose logs.

## Architecture

The crate builds:

- `meme-bot` (`src/bin/meme-bot.rs`): Telegram dispatcher, SQLite setup/migrations, queue worker, and user notifications in one process.
- `api-server` (`src/bin/api-server.rs`): axum REST API for Namecheap domain operations, DNS, and Cloudflare Pages deployment, protected by `X-API-Key`.

`src/lib.rs` exposes shared modules. The main user flow is: Telegram `/create` wizard -> SQLite order/payment records -> TX verification -> website record + generation job -> queue worker -> AI site generation -> GitHub repo push -> API server Cloudflare Pages deploy/domain bind -> Telegram notification.

Important module responsibilities:

- `config`: flat environment config via `Config::from_env()`.
- `models`: shared domain types such as `WebsiteInput`, `PaymentChain`, and `Order`.
- `validation`: regex validation for structured user inputs; user-provided prompt text is not accepted.
- `db`: raw SQLx queries and SQLite helpers.
- `payment`: `PaymentVerifier` boundary plus ERC20/TRON/SOL USDT verification and payment invariants.
- `generation`: `SiteGenerationProvider` boundary, `GenerationRouter` provider failover, and CLI-backed Gemini/Codex/Claude generation.
- `queue`: SQLite-backed generation job locking, retries, status transitions, deployment handoff, and user notifications.
- `deployment`: `SiteDeployer` boundary plus GitHub/Cloudflare Pages logic.
- `domain`: `DomainRegistrar` and `DnsProvisioner` boundaries for Namecheap/Cloudflare DNS.
- `telegram`: teloxide/dptree handlers, inline keyboards, payment/domain flows, and the manual template contact screen.

## Codebase conventions

- External integrations go behind traits with `#[async_trait]`, real implementations, and credential-free test stubs/dry-runs. Existing boundaries: `PaymentVerifier`, `SiteGenerationProvider`, `SiteDeployer`, `DomainRegistrar`, and `DnsProvisioner`. Wire shared implementations at the binary entry point with `Arc<dyn Trait>`.
- Use `anyhow::Result<T>` with `.context()` / `.with_context()` for general propagation. Use `thiserror` enums when callers need to match domain-specific failures, such as payment, generation, deployment, or domain errors.
- Generation failover is intentional: `GenerationRouter` tries the configured primary provider first and only falls back on recoverable provider errors (`QuotaExhausted`, `RateLimited`, `ProviderUnavailable`, invalid output).
- Configuration is environment-only. When adding a setting, update `Config`, read it in `Config::from_env()`, and document it in the matching `.env.*.example` file.
- SQLite uses `sqlx::SqlitePool` with `max_connections(1)`. Only `meme-bot` opens the database and runs migrations. Queue locking uses a `tokio::sync::Mutex` around `lock_next_generation`'s SELECT+UPDATE flow.
- IDs are UUID v4 stored as `TEXT`; money uses `rust_decimal::Decimal` and is stored as text in SQLite; timestamps use `chrono::DateTime<Utc>` / ISO 8601 text.
- Schema changes belong in new files under `migrations/` (for example `0002_*.sql`). Keep `PRAGMA foreign_keys = ON`, status `CHECK` constraints, and indexes for queried columns. Follow `.github/instructions/migrations.instructions.md`.
- Tests are colocated in module `#[cfg(test)]` blocks. Use sync `#[test]` or async `#[tokio::test]`; use stubs/dry-runs and `tempfile` rather than real external credentials.
- Production deployment is expected to go through `api-server` when `API_SERVICE_BASE_URL` and `API_SERVICE_API_KEY` are set. Without those, `meme-bot` should leave generated sites needing manual review rather than silently deploying directly.
- Do not commit `.env*` files except examples, API keys, bot tokens, SQLite databases, generated site artifacts, or other secrets.
