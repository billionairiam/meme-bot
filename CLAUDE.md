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

The Telegram bot runs in `meme-bot` and requires `TELEGRAM_BOT_TOKEN`. AI orders
are persisted to SQLite and picked up by the queue worker after payment.
Template mode is a separate manual offer: it displays a fixed 200 USDT price,
links to `https://memewebs.store`, and directs users to `ADMIN_CONTACT`; it does
not create an in-bot order.

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
Telegram user → /create → AI wizard → select payment chain
  → DB: upsert user + create order + attach payment chain
  → user submits TX hash
  → DB: mark order paid
  → queue worker: lock order → generate → deploy → mark done
```

### Module map

| Module | Role |
|---|---|
| `config` | Flat environment configuration loaded by `Config::from_env()`. |
| `models` | Shared order, payment-chain, website, and input domain types. |
| `validation` | Regex-based input validation for tickers, contract addresses (EVM/SOL/TRON), X/TG links, domains. No user-provided prompts — all input is structured fields. |
| `db` | SQLx stores for users, orders, and websites. |
| `payment` | `PaymentVerifier` boundary and ERC20/TRON/SOL USDT chain verifier. |
| `generation` | `SiteGenerationProvider` boundary and `GenerationRouter` CLI failover. |
| `queue` | `GenerationQueue` with SQLite-backed job locking (mutex-guarded SELECT + UPDATE). One `tick()` picks a queued job, generates, optionally deploys. Uses a `tokio::sync::Semaphore` for concurrency control. |
| `api_client` | `HttpClient*` structs that proxy to the API server. `HttpClientDomainRegistrar` implements `DomainRegistrar` (quote + purchase via `POST /api/v1/domain/*`). `HttpClientDnsProvisioner` (inherent methods: `create_zone`, `set_namecheap_nameservers`, `set_dns_records`; also implements the `DnsProvisioner` trait). `HttpClientPagesDeployer` implements `SiteDeployer` (creates GitHub repo, pushes site, calls `POST /api/v1/pages/deploy`). All use `X-API-Key` auth. |
| `deployment` | `src/deployment/{mod.rs, github.rs, cloudflare.rs}`. `SiteDeployer` trait + `CloudflarePagesDeployer` (direct Cloudflare API). `github.rs` handles repo creation and git push. `cloudflare.rs` has `create_cloudflare_zone()` (calls Cloudflare `POST /zones`), Pages project creation, deployment triggering, and domain binding. |
| `domain` | `src/domain/{mod.rs, registrar.rs, namecheap.rs, dns.rs, namecheap_xml.rs}`. `DomainRegistrar` trait with `NamecheapRegistrar` impl (check → getPricing → create). `DnsProvisioner` trait (single method: `set_namecheap_nameservers`) with `NamecheapDnsProvisioner` impl. `SUGGESTED_TLDS` in `registrar.rs` lists 19 TLDs for alternative-domain suggestions. |
| `telegram` | teloxide handlers, screens, keyboards, payment/domain flows, and helpers. |

### Key design patterns

- **Traits for external boundaries**: `PaymentVerifier`, `SiteGenerationProvider`, `SiteDeployer`, `DomainRegistrar`, and `DnsProvisioner`, with credential-free test doubles.
- **Generation provider failover**: `GenerationRouter` iterates providers in configured order. Recoverable errors (`QuotaExhausted`, `RateLimited`, `ProviderUnavailable`) trigger failover; non-recoverable errors stop immediately.
- **Config is flat env vars**: No config files. `Config::from_env()` reads everything. See `.env.meme-bot.example` and `.env.api-server.example` for all supported vars.
- **SQLite pool is single-connection** (`max_connections(1)`). The mutex in `lock_next_job` provides an additional guard, but the single connection ensures SQLite never sees concurrent writes.
- **API server proxy mode**: Production domain, DNS, and Pages operations use the authenticated API-server clients when `API_SERVICE_BASE_URL` and `API_SERVICE_API_KEY` are configured.
- **SQLite schema** is the ordered migration set under `migrations/`. Both binaries run pending SQLx migrations automatically; migration 0002 requires the documented database backup before deployment.
