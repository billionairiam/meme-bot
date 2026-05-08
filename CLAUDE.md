# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Build / Run / Test

```bash
cargo run                          # runs meme-bot (Telegram dispatcher + queue worker)
cargo run --bin api-server         # start API server
cargo test                         # run all tests
cargo test <test_name>             # run a single test (e.g. cargo test renders_wrangler_args)
cargo test -- --nocapture          # run tests with println/output visible
```

Set `RUST_LOG=debug` (or `trace`) to enable verbose logging — both binaries use `tracing-subscriber` with `EnvFilter`.

### Static musl release build (portable binary)

```bash
# Prerequisites (one-time):
rustup target add x86_64-unknown-linux-musl
sudo apt-get install -y musl-tools

# Build all binaries:
cargo build-musl-release           # alias defined in .cargo/config.toml
# or: cargo build --target x86_64-unknown-linux-musl --release

# Output binaries:
#   target/x86_64-unknown-linux-musl/release/meme-bot
#   target/x86_64-unknown-linux-musl/release/api-server
```

### Telegram bot

The Telegram bot runs as part of the `meme-bot` binary. Requires `TELEGRAM_BOT_TOKEN` and `TEMPLATE_CATALOG_URL`. All orders are persisted to SQLite (`./db.sqlite3`, created automatically on first run) — users select a payment chain → order written to DB, submit TX hash → order marked paid + generation job enqueued. The queue worker picks up jobs automatically.

## Architecture

Rust (edition 2024) Telegram bot that sells meme coin websites. Users pay USDT on-chain, the bot generates a static site via AI agents (Gemini/Codex CLI), deploys to Cloudflare Pages, and registers a domain via Namecheap.

### Two binaries, one crate

All binaries live under `src/bin/` and import from the `meme_bot` library crate (`src/lib.rs`):

| Binary | Entry point | Role |
|--------|-------------|------|
| `meme-bot` | `src/bin/meme-bot.rs` | Telegram bot + queue worker (two concurrent tasks, shared SqlitePool) |
| `api-server` | `src/bin/api-server.rs` | REST API for Namecheap/Cloudflare |

- **meme-bot**: `DATABASE_URL` defaults to `sqlite://./db.sqlite3` if unset. The binary creates the database file and runs SQLx migrations automatically, then spawns two concurrent tasks: the Telegram `Dispatcher` (long-running) and a queue worker loop (tick every 5s). The Telegram bot writes orders to DB at payment chain selection, marks them paid and enqueues generation jobs at TX submission. The queue worker picks up jobs using a mutex-guarded query.
- **API server** (`api-server`): No DB required → axum HTTP server with `X-API-Key` auth, serves Namecheap and Cloudflare operations.

### Data flow

```
Telegram user → /create → 7-step wizard → select payment chain
  → DB: upsert_user + create_site_order + attach_payment_chain
  → user submits TX hash
  → DB: mark_order_paid + create_website + enqueue_generation_job
  → queue worker: lock_next_job → generate → deploy → mark live
```

### Module map

| Module | Role |
|---|---|
| `config` | All configuration from env vars (see `.env.example`). `Config::from_env()` reads everything. |
| `models` | Shared domain types: `WebsiteInput`, `BuildMode`, `OrderType`, `PaymentChain`, `Order`, `Template`. |
| `validation` | Regex-based input validation for tickers, contract addresses (EVM/SOL/TRON), X/TG links, domains. No user-provided prompts — all input is structured fields. |
| `db` | Raw SQLx queries: `upsert_user`, `create_site_order`, `mark_order_paid`, `create_website`, `enqueue_generation_job`, `queue_position`. |
| `payment` | `PaymentVerifier` trait + chain verifier for ERC20/TRON/SOL USDT. `PaymentService` wraps provider checks with expiry, recipient, amount, confirmation, and transaction-age validation. Chain addresses are fixed per env config. |
| `generation` | `SiteGenerationProvider` trait. `GenerationRouter` tries providers in order (primary → fallback) on recoverable errors. `CliGenerationProvider` runs Gemini/Codex as external CLI processes with a 900s timeout, validates output via `.agent_done.json` + static file checks. `StaticTemplateProvider` exists for tests. |
| `queue` | `GenerationQueue` with SQLite-backed job locking (mutex-guarded SELECT + UPDATE). One `tick()` picks a queued job, generates, optionally deploys. Uses a `tokio::sync::Semaphore` for concurrency control. |
| `deployment` | `SiteDeployer` trait with four impls. `HttpClientPagesDeployer`: meme-bot creates GitHub repo + pushes code, then calls `POST /api/v1/pages/deploy` on the API server which creates the CF Pages project (git-connected), triggers deployment, and binds the custom domain. `CloudflarePagesDeployer` (fallback when `API_SERVICE_BASE_URL` is unset): does everything locally — creates repo, pushes, wires CF Pages (git integration if `GITHUB_TOKEN` is set, otherwise wrangler direct upload). `GitCliCloudflareDeployer`: git push only, no CF setup. `DryRunDeployer` for testing. |
| `domain` | `DomainRegistrar` trait. `NamecheapRegistrar` calls Namecheap XML API (check → getPricing → create). `DnsProvisioner` trait with `DryRunDnsProvisioner`. Supported TLDs: `.com`, `.net`, `.org`, `.xyz`, `.fun`, `.site`, `.online`, `.io`, `.co`, `.me`, `.cc`, `.app`, `.dev`, `.world`, `.space`, `.club`, `.store`, `.shop`. |
| `telegram` | teloxide-based bot with `dptree` dispatch. Inline keyboard UI flow: main menu → template/AI mode → 7-step form (logo, ticker, name, contract, X, TG, domain) → payment chain selection (writes order to DB) → TX hash submission (marks paid, enqueues generation job). When a requested domain is unavailable, queries all 18 supported TLDs for the same base name and shows available alternatives with prices. Template deep-links via `/start tpl_<id>`. Uses `SqlitePool` via dptree dependency injection. |
| `app` | Wires `GenerationRouter` + `GenerationQueue` + `SiteDeployer` for the queue worker. `build_queue()` creates a reusable `GenerationQueue` for the tick loop. |

### Key design patterns

- **Traits for external boundaries**: `PaymentVerifier`, `SiteGenerationProvider`, `SiteDeployer`, `DomainRegistrar`, `DnsProvisioner` — each has a stub/dry-run impl for testing and a real impl behind the trait.
- **Generation provider failover**: `GenerationRouter` iterates providers in configured order. Recoverable errors (`QuotaExhausted`, `RateLimited`, `ProviderUnavailable`) trigger failover; non-recoverable errors stop immediately.
- **Config is flat env vars**: No config files. `Config::from_env()` reads everything. See `.env.example` for all supported vars.
- **SQLite pool is single-connection** (`max_connections(1)`). The mutex in `lock_next_job` provides an additional guard, but the single connection ensures SQLite never sees concurrent writes.
- **API server proxy mode**: When `API_SERVICE_BASE_URL` is set, the bot uses `HttpClient*` trait impls that call the API server over HTTPS. When unset, it falls back to direct Namecheap/Cloudflare API calls — useful for single-server deployments.
- **`NAMECHEAP_AUTO_PURCHASE_AFTER_TX` defaults to `false`** in code (`.env.example` suggests `true`). When `false`, paid orders pause for manual review before domain purchase and generation.
- **SQLite schema** in `migrations/0001_init.sql`: `users`, `orders`, `websites`, `templates`, `payment_attempts`, `payment_transactions`, `generation_jobs`, `generation_attempts`, `domain_quotes`, `domain_registrations`, `deployment_attempts`.
