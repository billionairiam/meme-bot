# Meme Bot

Rust Telegram bot for selling meme coin websites. Users choose a template or AI-generated site, provide the required token basics, pay USDT on-chain, and the system generates a static website, pushes it to GitHub, deploys it with Cloudflare Pages, and optionally registers/configures a custom domain through Namecheap and Cloudflare.

## What Runs

This repository is one Rust 2024 crate with two binaries and shared library modules.

| Binary | Entry point | Role |
|--------|-------------|------|
| `meme-bot` | `src/bin/meme-bot.rs` | Telegram bot, order wizard, payment verification, generation queue worker |
| `api-server` | `src/bin/api-server.rs` | REST API for Namecheap and Cloudflare operations |

`cargo run` starts `meme-bot` by default. `cargo run --bin api-server` starts the API server.

## Architecture

```text
Telegram user
    |
    | Bot API
    v
meme-bot
    |-- SQLite orders/users/websites/templates
    |-- USDT payment verification
    |-- Gemini/Codex/Claude CLI generation
    |-- GitHub repo creation + push
    |
    | HTTPS + X-API-Key
    v
api-server
    |-- Namecheap XML API
    |-- Cloudflare DNS/Pages API
```

Recommended production deployment:

- Run `meme-bot` on the bot/home server.
- Run `api-server` on a VPS behind TLS and an IP allowlist.
- Keep Namecheap and Cloudflare credentials only on the API server.
- Give `meme-bot` only `API_SERVICE_BASE_URL`, `API_SERVICE_API_KEY`, and GitHub credentials for pushing generated sites.

If the API service variables are missing, generated sites can still be held for manual review instead of direct deployment.

## Project Layout

```text
mem-bot/
├── migrations/              # SQLite schema
├── prompts/                 # Agent prompt templates
├── scripts/                 # Utility scripts
├── src/
│   ├── bin/
│   │   ├── meme-bot.rs      # Telegram bot + queue worker
│   │   └── api-server.rs    # REST API server
│   ├── api_client.rs        # API-server client implementations
│   ├── app.rs               # App wiring
│   ├── config.rs            # Env configuration
│   ├── db.rs                # SQLite queries
│   ├── deployment/          # GitHub + Cloudflare Pages deployment
│   ├── domain/              # Namecheap + DNS traits/impls
│   ├── generation.rs        # AI agent providers and fallback router
│   ├── models.rs            # Shared domain models
│   ├── payment.rs           # On-chain USDT verification
│   ├── queue.rs             # SQLite-backed generation queue
│   ├── telegram/            # Telegram UI, handlers, keyboards
│   └── validation.rs        # User input validation
├── .env.meme-bot.example
├── .env.api-server.example
└── Cargo.toml
```

## Data Model

The SQLite schema is in `migrations/0001_init.sql`.

| Table | Purpose |
|-------|---------|
| `users` | Telegram user identity and membership state |
| `templates` | Template catalog rows |
| `orders` | Order wizard state, edit state, pricing, payment data, and queue lock fields |
| `websites` | Completed website/deployment records |

The order lifecycle is represented by one `orders.status` value, such as `need_chain`, `need_logo`, `need_pay`, `paid_and_generating`, `need_manual_review`, `done`, or `cancelled`.

## Bot Workflow

Creation supports template and AI modes. The required website inputs are chain (`ETH`, `BSC`, `SOL`, or `TON`), logo, token ticker, token name, and website domain. Contract address, X URL, and Telegram URL can be skipped; skipped values are stored as text placeholders:

- Contract: `0x0000000000000000000000000000000000000000`
- X: `https://x.com`
- Telegram: `https://t.me`

Live websites can be edited with `/edit`. Users may change only contract address, X URL, and Telegram URL. The bot applies edits by replacing the stored text in the matching GitHub repository and pushing a new commit; Cloudflare Pages updates from that commit. Each Telegram user gets three successful free edits globally. After that, each edit costs `10` USDT and uses the same payment verification flow.

## Quick Start

### Prerequisites

- Rust toolchain with edition 2024 support
- SQLite database file, created automatically from migrations
- Telegram bot token
- For production fulfillment: USDT RPC/API access, GitHub token, API server, Namecheap, and Cloudflare credentials

### Build and Test

```bash
cargo build
cargo test
cargo fmt
cargo clippy
```

Static Linux release binaries:

```bash
rustup target add x86_64-unknown-linux-musl
sudo apt-get install -y musl-tools
cargo build-musl-release
```

Output binaries:

```text
target/x86_64-unknown-linux-musl/release/meme-bot
target/x86_64-unknown-linux-musl/release/api-server
```

### Configure

```bash
cp .env.meme-bot.example .env.meme-bot
cp .env.api-server.example .env.api-server
```

Fill in real values locally. Do not commit `.env*` files except the checked-in `.example` files.

Default env files:

| Binary | Default env file | Override variable |
|--------|------------------|-------------------|
| `meme-bot` | `.env.meme-bot` | `MEME_BOT_ENV_FILE=/path/to/env` |
| `api-server` | `.env.api-server` | `API_SERVER_ENV_FILE=/path/to/env` |

### Run

```bash
# Telegram bot + queue worker
cargo run

# API server
cargo run --bin api-server
```

`DATABASE_URL` defaults to `sqlite://./db.sqlite3`. For local development the example file uses `sqlite://./db.sqlite3?mode=rwc`.

## meme-bot Configuration

Important variables in `.env.meme-bot`:

| Variable | Notes |
|----------|-------|
| `TELEGRAM_BOT_TOKEN` | Telegram bot token |
| `TELEGRAM_REQUIRED_GROUP_ID` | Group/channel membership gate |
| `TEMPLATE_PRICES_USDT` | 12 comma-separated template prices |
| `TEMPLATE_PREVIEW_DOMAIN` | Suffix for numbered preview domains |
| `TEMPLATE_PREVIEW_BASE_URL` | Base URL for preview links |
| `AI_CREATION_PRICE_USDT` | AI-generated site price |
| `USDT_ERC20_ADDRESS` / `USDT_TRON_ADDRESS` / `USDT_SOL_ADDRESS` | Receiving addresses |
| `USDT_ERC20_RPC_URL` / `TRON_GRID_BASE_URL` / `SOLANA_RPC_URL` | Payment verification providers |
| `GENERATION_PRIMARY_PROVIDER` | `gemini`, `codex`, or `claude` |
| `GENERATION_MAX_CONCURRENCY` | Queue concurrency limit |
| `AGENT_TIMEOUT_SECONDS` | External agent timeout |
| `GENERATED_SITES_DIR` | Output root for generated websites |
| `LOCAL_TEMPLATES_DIR` | Local template source root |
| `GITHUB_OWNER` / `GITHUB_TOKEN` | GitHub owner/token used by the bot |
| `API_SERVICE_BASE_URL` / `API_SERVICE_API_KEY` | API server connection for domain/DNS/Pages operations |
| `AUTO_PURCHASE_AFTER_TX` | Continue fulfillment after payment confirmation |

Testing-only switches:

```bash
SKIP_PAYMENT_VERIFICATION=false
SKIP_DOMAIN_PURCHASE=false
```

Never enable those in production.

## API Server

The API server exposes Namecheap and Cloudflare operations under `/api/v1`. All endpoints except health require:

```text
X-API-Key: <API_SERVICE_API_KEY>
```

Generate a key:

```bash
./scripts/generate-api-key.sh
# or
openssl rand -base64 48
```

Important variables in `.env.api-server`:

| Variable | Notes |
|----------|-------|
| `API_SERVICE_API_KEY` | Required, at least 32 characters |
| `API_SERVICE_BIND_ADDR` | Defaults to `127.0.0.1` |
| `API_SERVICE_PORT` | Defaults to `8080` |
| `NAMECHEAP_API_USER` | Namecheap API user |
| `NAMECHEAP_API_KEY` | Namecheap API key |
| `NAMECHEAP_USERNAME` | Namecheap account username |
| `NAMECHEAP_CLIENT_IP` | Whitelisted server public IP |
| `NAMECHEAP_SANDBOX` | Defaults to `true` |
| `NAMECHEAP_CONTACT_*` | Registration contact fields |
| `CLOUDFLARE_ACCOUNT_ID` | Cloudflare account ID |
| `CLOUDFLARE_API_TOKEN` | Cloudflare token |

### Endpoints

| Method | Path | Auth | Purpose |
|--------|------|------|---------|
| `GET` | `/api/v1/health` | No | Liveness check |
| `POST` | `/api/v1/domain/quote` | Yes | Check availability and price |
| `POST` | `/api/v1/domain/purchase` | Yes | Register a domain |
| `POST` | `/api/v1/dns/create-zone` | Yes | Create/find Cloudflare zone and return nameservers |
| `POST` | `/api/v1/dns/zone-status` | Yes | Read Cloudflare zone status |
| `POST` | `/api/v1/dns/set-nameservers` | Yes | Set Namecheap nameservers |
| `POST` | `/api/v1/pages/deploy` | Yes | Create/verify Pages project, deploy, optionally bind domain |

Example health check:

```bash
curl http://127.0.0.1:8080/api/v1/health
```

Example authenticated request:

```bash
curl -X POST http://127.0.0.1:8080/api/v1/domain/quote \
  -H "Content-Type: application/json" \
  -H "X-API-Key: <your-api-key>" \
  -d '{"domain":"example.xyz"}'
```

Error responses use this shape:

```json
{"error":"<error_code>","message":"<human-readable description>"}
```

## Fulfillment Flow

1. User starts the Telegram wizard with `/start` or `/create`.
2. User chooses template mode or AI creation mode.
3. Bot collects logo, ticker, token name, contract, X link, Telegram link, and domain.
4. Bot quotes/registers the domain when configured.
5. User selects payment chain and submits a transaction hash.
6. Payment service verifies recipient, amount, confirmations, and transaction time.
7. Order becomes `paid_and_generating`.
8. Queue worker locks the order, runs the configured AI agent, pushes the site to GitHub, and asks `api-server` to deploy it.
9. Successful deployment creates a `websites` row and marks the order `done`.

Supported user commands include:

```text
/start
/help
/create
/continue
/status
/history
/cancel
```

## Agent Generation

The queue worker runs external CLI tools in non-interactive mode. The provider order is controlled by `GENERATION_PRIMARY_PROVIDER`; recoverable provider failures fall back to the other configured providers.

Default command examples:

```bash
GEMINI_AGENT_COMMAND=gemini
GEMINI_AGENT_ARGS="--yolo --skip-trust -p {prompt}"

CODEX_AGENT_COMMAND=codex
CODEX_AGENT_ARGS="exec --dangerously-bypass-approvals-and-sandbox {prompt}"

CLAUDE_AGENT_COMMAND=claude
CLAUDE_AGENT_ARGS="-p {prompt} --output-format stream-json --verbose --allowedTools Read Edit Write Bash --dangerously-skip-permissions"
```

The generation prompts live in:

```text
prompts/agent-gemini.md
prompts/agent-codex.md
prompts/agent-claude.md
```

## Production Notes

For the API server:

- Bind the service to `127.0.0.1`.
- Put Caddy or nginx in front for TLS.
- Restrict access to the bot server IP.
- Keep port `8080` private.
- Store `.env.api-server` with restrictive permissions.

Example systemd service:

```ini
[Unit]
Description=Meme Bot API Server
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=api-server
Environment=API_SERVER_ENV_FILE=/etc/api-server/.env
ExecStart=/opt/api-server/api-server
Restart=on-failure
RestartSec=5

[Install]
WantedBy=multi-user.target
```

For the bot server, set:

```bash
API_SERVICE_BASE_URL=https://api.example.com
API_SERVICE_API_KEY=<same-key-as-api-server>
```

## Security

- Do not commit `.env*` files, API keys, bot tokens, SQLite databases, or generated site artifacts.
- Use `rust_decimal::Decimal` for money; never use `f64`.
- Keep external integrations behind traits so tests can use stubs/dry-run implementations.
- Run `cargo test` before opening a PR.
