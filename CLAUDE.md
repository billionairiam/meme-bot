# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Build / Run / Test

First-time setup:

```bash
cp .env.meme-bot.example .env.meme-bot
cp .env.api-server.example .env.api-server
# edit both .env files with real values
```

```bash
cargo build                        # debug build
cargo run                          # runs meme-bot (Telegram dispatcher + queue worker)
cargo run --bin api-server         # start API server
cargo test                         # run all tests
cargo test <test_name>             # run a single test (e.g. cargo test renders_wrangler_args)
cargo test -- --nocapture          # run tests with println/output visible
cargo clippy                       # lint
cargo fmt --check                  # check formatting
cargo fmt                          # format code
```

`meme-bot` loads `.env.meme-bot` by default; `api-server` loads `.env.api-server` by default. Override with `MEME_BOT_ENV_FILE` or `API_SERVER_ENV_FILE`.

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

### Project scripts

- `scripts/generate-api-key.sh` — generates a random API key via openssl for `API_SERVICE_API_KEY`.
- `scripts/set_telegram_commands.sh` — registers the bot's slash-command menu with Telegram via `setMyCommands`.

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
| `config` | All configuration from env vars. `Config::from_env()` reads everything. Includes template preview (`TEMPLATE_PREVIEW_DOMAIN`, `TEMPLATE_PREVIEW_BASE_URL`). See `.env.meme-bot.example` and `.env.api-server.example`. |
| `models` | Shared domain types: `WebsiteInput`, `BuildMode`, `OrderType`, `PaymentChain`, `Order`, `Template`. |
| `validation` | Regex-based input validation for tickers, contract addresses (EVM/SOL/TRON), X/TG links, domains. No user-provided prompts — all input is structured fields. |
| `db` | Raw SQLx queries: `upsert_user`, `create_site_order`, `mark_order_paid`, `create_website`, `enqueue_generation_job`, `queue_position`. |
| `payment` | `PaymentVerifier` trait + chain verifier for ERC20/TRON/SOL USDT. `PaymentService` wraps provider checks with expiry, recipient, amount, confirmation, and transaction-age validation. Chain addresses are fixed per env config. |
| `generation` | `SiteGenerationProvider` trait. `GenerationRouter` tries providers in order (primary → fallback) on recoverable errors. `CliGenerationProvider` runs Gemini/Codex/Claude as external CLI processes with a 900s timeout, validates output via `agent-prompt.md` + `.agent_done.json` + static file checks. Prompt templates live in `prompts/agent-{provider}.md`. `StaticTemplateProvider` exists for tests. |
| `queue` | `GenerationQueue` with SQLite-backed job locking (mutex-guarded SELECT + UPDATE). One `tick()` picks a queued job, generates, optionally deploys. Uses a `tokio::sync::Semaphore` for concurrency control. |
| `api_client` | `HttpClient*` structs that proxy to the API server. `HttpClientDomainRegistrar` implements `DomainRegistrar` (quote + purchase via `POST /api/v1/domain/*`). `HttpClientDnsProvisioner` (inherent methods: `create_zone`, `set_namecheap_nameservers`, `set_dns_records`; also implements the `DnsProvisioner` trait). `HttpClientPagesDeployer` implements `SiteDeployer` (creates GitHub repo, pushes site, calls `POST /api/v1/pages/deploy`). All use `X-API-Key` auth. |
| `deployment` | `src/deployment/{mod.rs, github.rs, cloudflare.rs}`. `SiteDeployer` trait + `CloudflarePagesDeployer` (direct Cloudflare API). `github.rs` handles repo creation and git push. `cloudflare.rs` has `create_cloudflare_zone()` (calls Cloudflare `POST /zones`), Pages project creation, deployment triggering, and domain binding. |
| `domain` | `src/domain/{mod.rs, registrar.rs, namecheap.rs, dns.rs, namecheap_xml.rs}`. `DomainRegistrar` trait with `NamecheapRegistrar` impl (check → getPricing → create). `DnsProvisioner` trait (single method: `set_namecheap_nameservers`) with `NamecheapDnsProvisioner` impl. `SUGGESTED_TLDS` in `registrar.rs` lists 19 TLDs for alternative-domain suggestions. |
| `telegram` | `src/telegram/{mod.rs, handlers.rs, screens.rs, keyboards.rs, domain_flow.rs, payment.rs, helpers.rs, types.rs}`. teloxide-based bot with `dptree` dispatch. Inline keyboard UI flow: main menu → template/AI mode → 7-step form (logo, ticker, name, contract, X, TG, domain) → payment chain selection (writes order to DB) → TX hash submission (marks paid, enqueues generation job). When a requested domain is unavailable, queries all 19 supported TLDs for the same base name and shows available alternatives with prices. Template deep-links via `/start tpl_<id>`. Uses `SqlitePool` via dptree dependency injection. |
| `app` | Wires `GenerationRouter` + `GenerationQueue` + `SiteDeployer` for the queue worker. `build_queue()` creates a reusable `GenerationQueue` for the tick loop. |

### Key design patterns

- **Traits for external boundaries**: `PaymentVerifier`, `SiteGenerationProvider`, `SiteDeployer`, `DomainRegistrar`, `DnsProvisioner` — each has a real impl and an API-server-proxy impl (`HttpClient*` in `api_client`). `DnsProvisioner` is minimal (one method); `create_zone` lives as a free function in `deployment/cloudflare.rs` since it's a Cloudflare-specific operation.
- **Generation provider failover**: `GenerationRouter` iterates providers in configured order. Recoverable errors (`QuotaExhausted`, `RateLimited`, `ProviderUnavailable`) trigger failover; non-recoverable errors stop immediately.
- **Config is flat env vars**: No config files. `Config::from_env()` reads everything. See `.env.meme-bot.example` and `.env.api-server.example` for all supported vars.
- **SQLite pool is single-connection** (`max_connections(1)`). The mutex in `lock_next_job` provides an additional guard, but the single connection ensures SQLite never sees concurrent writes.
- **API server proxy mode**: When `API_SERVICE_BASE_URL` is set, the bot uses `HttpClient*` trait impls from `api_client` that call the API server over HTTPS. When unset, it falls back to direct Namecheap/Cloudflare API calls — useful for single-server deployments.
- **`NAMECHEAP_AUTO_PURCHASE_AFTER_TX` defaults to `false`** in code (`.env.meme-bot.example` suggests `true`). When `false`, paid orders pause for manual review before domain purchase and generation.
- **SQLite schema** lives in `migrations/0001_init.sql` (single file). Contains all tables: `users`, `orders`, `websites`, `templates`, `payment_attempts`, `payment_transactions`, `generation_jobs`, `generation_attempts`, `domain_quotes`, `domain_registrations`, `deployment_attempts`. Both binaries run SQLx migrations automatically on startup via `sqlx::migrate!()`.
