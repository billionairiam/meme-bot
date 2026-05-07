# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Build / Run / Test

```bash
cargo run                          # runs migrations, then one generation queue tick
cargo test                         # run all tests
```

### Static musl release build (portable binary)

```bash
# Prerequisites (one-time):
rustup target add x86_64-unknown-linux-musl
sudo apt-get install -y musl-tools

# Build:
cargo build-musl-release           # alias defined in .cargo/config.toml
# or: cargo build --target x86_64-unknown-linux-musl --release

# Output: target/x86_64-unknown-linux-musl/release/meme-bot
# The binary is statically linked — copy it to any Linux x86_64 machine and run.
```

### Frontend-only mode (no PostgreSQL required)

Set `TELEGRAM_FRONTEND_ONLY=true` (or omit `DATABASE_URL`) to run just the Telegram bot UI with in-memory session state. Also needs `TELEGRAM_BOT_TOKEN` and `TEMPLATE_CATALOG_URL`. The bot starts a long-running dispatcher (`Dispatcher::dispatch()`) with inline keyboard navigation, not a single tick.

## Architecture

Rust (edition 2024) Telegram bot that sells meme coin websites. Users pay USDT on-chain, the bot generates a static site via AI agents (Gemini/Codex CLI), deploys to Cloudflare Pages, and registers a domain via Namecheap.

### Two operating modes, decided in `main.rs`

- **Full backend**: `DATABASE_URL` is set → PostgreSQL pool, runs SQLx migrations, then `App::run_once()` which does one generation queue tick.
- **Frontend-only**: `TELEGRAM_FRONTEND_ONLY=true` or no `DATABASE_URL` → `telegram::run_frontend()` starts a teloxide `Dispatcher` with in-memory `HashMap<UserId, OrderDraft>` sessions. No database writes.

### Module map

| Module | Role |
|---|---|
| `config` | All configuration from env vars (see `.env.example`). `Config::from_env()` reads everything. |
| `models` | Shared domain types: `WebsiteInput`, `BuildMode`, `OrderType`, `PaymentChain`, `Order`, `Template`. |
| `validation` | Regex-based input validation for tickers, contract addresses (EVM/SOL/TRON), X/TG links, domains. No user-provided prompts — all input is structured fields. |
| `db` | Raw SQLx queries: `upsert_user`, `create_site_order`, `mark_order_paid`, `create_website`, `enqueue_generation_job`, `queue_position`. |
| `payment` | `PaymentVerifier` trait + `StubPaymentVerifier` (accepts `valid_*` TX hashes). `PaymentService` wraps a verifier with expiry/receiver/amount checks. Chain addresses are fixed per env config. |
| `generation` | `SiteGenerationProvider` trait. `GenerationRouter` tries providers in order (primary → fallback) on recoverable errors. `CliGenerationProvider` runs Gemini/Codex as external CLI processes with a 900s timeout, validates output via `.agent_done.json` + static file checks. `StaticTemplateProvider` exists for tests. |
| `queue` | `GenerationQueue` with PostgreSQL-backed job locking (`FOR UPDATE SKIP LOCKED`). One `tick()` picks a queued job, generates, optionally deploys. Uses a `tokio::sync::Semaphore` for concurrency control. |
| `deployment` | `SiteDeployer` trait. `CloudflarePagesDeployer` uses Cloudflare API (ensure project → wrangler direct upload → bind custom domain). `GitCliCloudflareDeployer` does git init/push to GitHub. `DryRunDeployer` for testing. |
| `domain` | `DomainRegistrar` trait. `NamecheapRegistrar` calls Namecheap XML API (check → getPricing → create). `DnsProvisioner` trait with `DryRunDnsProvisioner`. Supported TLDs: `.com`, `.xyz`, `.fun`, `.site`, `.online`. |
| `telegram` | teloxide-based bot with `dptree` dispatch. Inline keyboard UI flow: main menu → template/AI mode → 7-step form (logo, ticker, name, contract, X, TG, domain) → payment chain selection → TX hash submission. Template deep-links via `/start tpl_<id>`. |
| `app` | Wires `GenerationRouter` + `GenerationQueue` + optional `CloudflarePagesDeployer` for the `run_once()` backend mode. |

### Key design patterns

- **Traits for external boundaries**: `PaymentVerifier`, `SiteGenerationProvider`, `SiteDeployer`, `DomainRegistrar`, `DnsProvisioner` — each has a stub/dry-run impl for testing and a real impl behind the trait. This lets frontend-only mode skip real payment/chain verification.
- **Generation provider failover**: `GenerationRouter` iterates providers in configured order. Recoverable errors (`QuotaExhausted`, `RateLimited`, `ProviderUnavailable`) trigger failover; non-recoverable errors stop immediately.
- **Config is flat env vars**: No config files. `Config::from_env()` reads everything. See `.env.example` for all supported vars.
- **PostgreSQL schema** in `migrations/0001_init.sql`: `users`, `orders`, `websites`, `templates`, `payment_attempts`, `payment_transactions`, `generation_jobs`, `generation_attempts`, `domain_quotes`, `domain_registrations`, `deployment_attempts`.
