# Meme Bot

Rust monorepo for a Telegram bot that sells meme coin websites. Users pay USDT on-chain, the bot generates a static site via AI agents (Gemini/Codex CLI), deploys to Cloudflare Pages (Git integration), and registers a domain via Namecheap.

## Architecture

```
                      ┌──────────────────────────────┐
                      │         Namecheap            │
                      │         (XML API)            │
                      └────────▲─────────────────────┘
                               │
                               │ server-side API call
                               │
┌──────────────┐     ┌────────┴───────┐     ┌─────────────┐
│  Telegram    │     │   api-server   │     │  Cloudflare │
│  (user chat) │     │   (VPS)        │     │  (REST API) │
└──────┬───────┘     └────────▲───────┘     └─────────────┘
       │                      │
       │ Bot API               │ X-API-Key (TLS)
       │                      │
┌──────┴──────────────────────┴───────┐     ┌─────────────┐
│             meme-bot                │────▶│   SQLite    │
│          (home server)              │     │ (db.sqlite3)│
│  ┌────────────┐  ┌──────────────┐   │     └─────────────┘
│  │  Telegram  │  │ Queue Worker │   │
│  │ Dispatcher │  │  (tick loop) │   │
│  └────────────┘  └──────────────┘   │
└─────────────────────────────────────┘
```

### Two binaries, one crate

| Binary | Entry point | Role | Runs on |
|--------|-------------|------|---------|
| `meme-bot` | `src/bin/meme-bot.rs` | Telegram bot + queue worker (all-in-one) | Home server |
| `api-server` | `src/bin/api-server.rs` | REST API for Namecheap + Cloudflare operations | VPS |

Both compile from the same crate (`src/lib.rs`). The `meme-bot` binary runs the Telegram dispatcher and generation queue worker concurrently in a single process. All user orders are persisted to SQLite (`./db.sqlite3`, created automatically on first run) — no in-memory state that could be lost on restart.

`meme-bot` generates sites, creates the GitHub repo, and pushes the website code. Cloudflare deployment and custom-domain binding are delegated to `api-server` through `API_SERVICE_BASE_URL`/`API_SERVICE_API_KEY`; without those variables, paid generated sites are held for manual review instead of being deployed directly.

### Module map

| Module | Role |
|---|---|
| `config` | Flat env var configuration (`Config::from_env()`). See `.env.meme-bot.example` and `.env.api-server.example`. |
| `models` | Shared domain types: `WebsiteInput`, `BuildMode`, `OrderType`, `PaymentChain`, `Order`, `Template`. |
| `validation` | Regex-based input validation for tickers, contract addresses (EVM/SOL/TRON), X/TG links, domains. No user-provided prompts. |
| `db` | Raw SQLx queries: `upsert_user`, `create_site_order`, `mark_order_paid`, `enqueue_generation_job`, `queue_position`. |
| `payment` | `PaymentVerifier` trait + real chain verifier for ERC20/TRON/SOL USDT. `PaymentService` checks expiry, recipient, amount, confirmations, and transaction age. |
| `generation` | `SiteGenerationProvider` trait. `GenerationRouter` tries primary → fallback on recoverable errors. `CliGenerationProvider` runs Gemini/Codex as external CLI processes. |
| `queue` | `GenerationQueue` with SQLite-backed job locking (mutex-guarded SELECT + UPDATE), global semaphore for concurrency control. |
| `deployment` | `SiteDeployer` trait. `HttpClientPagesDeployer` lets `meme-bot` create/push GitHub repos, then asks `api-server` to deploy via Cloudflare Pages. `meme-bot` must not deploy directly to Cloudflare in production. |
| `domain` | `DomainRegistrar` trait. `NamecheapRegistrar` calls Namecheap XML API. `HttpClientDomainRegistrar` proxies through the API server. `DnsProvisioner` trait with `NamecheapDnsProvisioner` / `HttpClientDnsProvisioner`. |
| `telegram` | teloxide-based bot with `dptree` dispatch. Inline keyboard UI for 7-step order form, template deep-links (`/start tpl_<id>`). |
| `app` | Wires `GenerationRouter` + `GenerationQueue` + deployer into `run_once()` for backend mode. |

### Data flow

1. User interacts with Telegram bot → 7-step order wizard
2. User selects payment chain → order written to SQLite (`orders` table, status: `pending`)
3. User submits TX hash → order marked `paid`, website created, generation job enqueued
4. Queue worker picks up job (mutex-guarded query) → runs AI agent → pushes generated site to GitHub → creates/binds Cloudflare Pages
5. User checks `/status` → reads order status from SQLite

Users are limited to one unpaid order at a time. `/create` is blocked when a pending order exists; use `/continue` to resume it (the session is reconstructed from the DB if the in-memory state was lost on restart), or `/cancel` to give up. `/status` checks queue position; `/history` lists paid orders.

## Quick Start

### Prerequisites

- Rust toolchain (edition 2024)
- SQLite (bundled via libsqlite3-sys, no system dependency)

### Build

```bash
# Debug build
cargo build

# Static musl release build (portable binary for Linux x86_64)
rustup target add x86_64-unknown-linux-musl
sudo apt-get install -y musl-tools
cargo build-musl-release   # alias in .cargo/config.toml
# Output: target/x86_64-unknown-linux-musl/release/meme-bot
#         target/x86_64-unknown-linux-musl/release/api-server
```

```bash
cargo test    # run all tests
```

### Run

```bash
cp .env.meme-bot.example .env.meme-bot
cp .env.api-server.example .env.api-server

# Main binary (Telegram bot + queue worker, DATABASE_URL defaults to sqlite://./db.sqlite3)
cargo run

# API server
cargo run --bin api-server
```

`meme-bot` loads `.env.meme-bot` by default. `api-server` loads `.env.api-server` by default. Override with `MEME_BOT_ENV_FILE=/path/to/bot.env` or `API_SERVER_ENV_FILE=/path/to/api.env`.

## API Server

The `api-server` binary exposes Namecheap and Cloudflare operations behind a REST API with API key authentication. It is designed to run on a VPS behind a reverse proxy (Caddy/nginx) that handles TLS termination.

### Endpoints

All under `/api/v1/`. Auth: `X-API-Key` header (except health).

#### Domain (Namecheap)

| Method | Path | Description |
|--------|------|-------------|
| POST | `/domain/quote` | Check domain availability and get price |
| POST | `/domain/purchase` | Register a domain |

#### DNS

| Method | Path | Description |
|--------|------|-------------|
| POST | `/dns/create-zone` | Create a Cloudflare zone for a domain |
| POST | `/dns/set-nameservers` | Set Namecheap nameservers for a domain |
| POST | `/dns/set-records` | Set DNS records on Namecheap |

#### Cloudflare Pages (Git Integration)

| Method | Path | Description |
|--------|------|-------------|
| POST | `/pages/deploy` | Create a Cloudflare Pages project connected to a GitHub repo, optionally bind a custom domain |

#### Health

| Method | Path | Description |
|--------|------|-------------|
| GET | `/api/v1/health` | Returns `{"status":"ok"}` |

### Error format

```json
{"error": "<error_code>", "message": "<human-readable description>"}
```

HTTP status codes: `400` client error, `500` server config error, `502` upstream API error.

### API key generation and validation

The API key is a shared secret that both the API server and the bot must know. No OAuth, no JWT — just a long random string compared with constant-time-like string equality.

**Generate a key** (on any machine with openssl):

```bash
# Option A — use the convenience script:
./scripts/generate-api-key.sh

# Option B — manually:
openssl rand -base64 48
```

This produces a 64-character random string. Save it somewhere secure (password manager, vault).

**How validation works:** Every request (except `/api/v1/health`) must include the header `X-API-Key: <key>`. The server's `auth_middleware` extracts this header and compares it against the `API_SERVICE_API_KEY` env var value the server was started with. If they don't match, the server returns `401 Unauthorized` with:

```json
{"error": "unauthorized", "message": "invalid API key"}
```

The health endpoint (`GET /api/v1/health`) is the only unauthenticated path — used by monitoring tools and load balancers to check liveness.

### API server environment variables

| Var | Required | Default | Notes |
|-----|----------|---------|-------|
| `API_SERVICE_API_KEY` | **Yes** | — | Min 32 characters (see generation above) |
| `API_SERVICE_PORT` | No | `8080` | |
| `API_SERVICE_BIND_ADDR` | No | `127.0.0.1` | `0.0.0.0` to listen on all interfaces |
| `NAMECHEAP_API_USER` | Yes | — | |
| `NAMECHEAP_API_KEY` | Yes | — | |
| `NAMECHEAP_USERNAME` | Yes | — | |
| `NAMECHEAP_CLIENT_IP` | Yes | — | Server public IP (whitelisted in Namecheap) |
| `NAMECHEAP_SANDBOX` | No | `true` | Set to `false` for production |
| `NAMECHEAP_CONTACT_*` | For purchase | — | See Namecheap section below |
| `CLOUDFLARE_ACCOUNT_ID` | For CF ops | — | |
| `CLOUDFLARE_API_TOKEN` | For CF ops | — | |

### Run the API server

**Option A — Quick start with `.env.api-server` (development / testing):**

Create `.env.api-server` in the project root (or copy from `.env.api-server.example`):

```bash
API_SERVICE_API_KEY=<output from openssl rand -base64 48>
API_SERVICE_PORT=8080
API_SERVICE_BIND_ADDR=127.0.0.1

# Namecheap
NAMECHEAP_API_USER=your_api_user
NAMECHEAP_API_KEY=your_api_key
NAMECHEAP_USERNAME=your_username
NAMECHEAP_CLIENT_IP=<VPS public IP>
NAMECHEAP_SANDBOX=true
NAMECHEAP_CONTACT_FIRST_NAME=...
# ... (other NAMECHEAP_CONTACT_* fields)

# Cloudflare
CLOUDFLARE_ACCOUNT_ID=...
CLOUDFLARE_API_TOKEN=...
```

Then run:

```bash
cargo run --bin api-server
```

The server reads `.env.api-server` automatically. Set `API_SERVER_ENV_FILE=/path/to/api.env` to use a different file.

**Option B — Prod with systemd:**

Build the static musl binary:

```bash
cargo build-musl-release
# Binary at: target/x86_64-unknown-linux-musl/release/api-server
```

Copy the binary to the VPS, then create `/etc/systemd/system/api-server.service`:

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

```bash
sudo useradd -r -s /usr/sbin/nologin api-server
sudo mkdir -p /opt/api-server /etc/api-server
sudo cp target/x86_64-unknown-linux-musl/release/api-server /opt/api-server/
sudo vim /etc/api-server/.env    # paste env vars
sudo chmod 600 /etc/api-server/.env
sudo systemctl daemon-reload
sudo systemctl enable --now api-server
sudo systemctl status api-server
```

If SELinux is enforcing (RHEL / AlmaLinux / CentOS), allow the service to run from `/opt` and read config from `/etc`:

```bash
sudo chcon -t bin_t /opt/api-server/api-server
sudo chcon -t etc_t /etc/api-server/.env
```

Or configure a proper SELinux policy module if preferred.

**Verify it's running:**

```bash
# Health check (no auth required)
curl http://127.0.0.1:8080/api/v1/health
# → {"status":"ok"}

# Authenticated endpoint (should 401 without key)
curl -X POST http://127.0.0.1:8080/api/v1/domain/quote \
  -H "Content-Type: application/json" \
  -d '{"domain":"test.xyz"}'
# → {"error":"unauthorized","message":"invalid API key"}

# With correct key
curl -X POST http://127.0.0.1:8080/api/v1/domain/quote \
  -H "Content-Type: application/json" \
  -H "X-API-Key: <your-key>" \
  -d '{"domain":"test.xyz"}'
# → {"domain":"test.xyz","available":...,"price":...,"currency":"USD","tld":"xyz",...}
```

### Reverse proxy (Caddy)

The API server binds to `127.0.0.1` by default — only local processes can reach it. Caddy sits in front and handles TLS termination, rate limiting, and IP allowlisting.

#### Install Caddy

**Debian / Ubuntu:**

```bash
sudo apt install -y debian-keyring debian-archive-keyring apt-transport-https
curl -1sLf 'https://dl.cloudflare.com/caddy/gpg.key' | sudo gpg --dearmor -o /usr/share/keyrings/caddy-stable-archive-keyring.gpg
curl -1sLf 'https://dl.cloudflare.com/caddy/debian/deb.repo' | sudo tee /etc/apt/sources.list.d/caddy-stable.list
sudo apt update
sudo apt install caddy
```

**RHEL / AlmaLinux / CentOS:**

```bash
sudo dnf install -y epel-release
sudo dnf install -y 'dnf-command(copr)'
sudo dnf copr enable -y @caddy/caddy
sudo dnf install -y caddy
```

#### Configure

Edit `/etc/caddy/Caddyfile`:

```caddyfile
# Global options block (optional — set default email for ACME)
{
    email your@email.com
}

# API server reverse proxy
api.yourdomain.com {
    # Rate limiting: 10 requests per second per IP, burst 20
    rate_limit {
        zone dynamic {
            key {remote_host}
            events 10 20
            window 1s
        }
    }

    # IP allowlist — only your home server can reach the backend
    @allowed remote_ip <home-server-public-IP>
    handle @allowed {
        reverse_proxy 127.0.0.1:8080
    }
    handle {
        respond "Access denied" 403
    }

    # Request logging
    log {
        output file /var/log/caddy/api-access.log
    }
}
```

**Notes:**
- Replace `api.yourdomain.com` with your actual API domain (e.g., `api.mymemebot.com`)
- Replace `<home-server-public-IP>` with your home server's public IPv4 (run `curl -4 ifconfig.me` on the home server to find it)
- Caddy auto-provisions and renews Let's Encrypt certificates — no extra steps needed
- Ports 80 and 443 must be open on the VPS firewall
- DNS A/AAAA record for `api.yourdomain.com` must point to the VPS public IP

#### Start

```bash
# Validate config
sudo caddy validate --config /etc/caddy/Caddyfile

# Start
sudo systemctl enable --now caddy
sudo systemctl status caddy
```

#### Verify

```bash
# Health check (no auth)
curl https://api.yourdomain.com/api/v1/health
# → {"status":"ok"}

# Authenticated endpoint (should 401 without key)
curl -X POST https://api.yourdomain.com/api/v1/domain/quote \
  -H "Content-Type: application/json" \
  -d '{"domain":"test.xyz"}'
# → {"error":"unauthorized","message":"invalid API key"}

# From a non-whitelisted IP (should 403)
# (run this from a machine that is NOT the home server)
curl https://api.yourdomain.com/api/v1/health
# → Access denied
```

#### Firewall

Allow only ports 80 and 443 from the internet. The API server port (8080) should NOT be exposed.

**Debian / Ubuntu (ufw):**

```bash
sudo ufw allow 80/tcp
sudo ufw allow 443/tcp
sudo ufw deny 8080/tcp   # just to be explicit — default deny covers this
sudo ufw enable
```

**RHEL / AlmaLinux / CentOS (firewalld):**

```bash
sudo firewall-cmd --permanent --add-service=http
sudo firewall-cmd --permanent --add-service=https
sudo firewall-cmd --reload
```

## Connecting the bot to the API server

Set these env vars on the bot (home server):

```bash
API_SERVICE_BASE_URL=https://api.yourdomain.com
API_SERVICE_API_KEY=<same-key-as-above>
```

Both variables are required for production. The bot uses `HttpClientDomainRegistrar`, `HttpClientDnsProvisioner`, and `HttpClientPagesDeployer` instead of direct Namecheap/Cloudflare calls. Namecheap and Cloudflare credentials belong on the API server, not on the bot server. If the Pages API service is missing, generated sites are marked for manual review instead of being deployed directly by `meme-bot`.

### Security model

- **TLS everywhere**: Caddy/nginx terminates TLS with Let's Encrypt. The home server connects to the VPS over HTTPS.
- **API key auth**: Every non-health request must carry `X-API-Key` matching the server's `API_SERVICE_API_KEY`. Mismatch → `401`.
- **IP allowlist**: Caddy `remote_ip` matcher restricts which IPs can even reach the backend. Only your home server's public IP is allowed.
- **No public ports on the service**: The API server binds to `127.0.0.1` — only the reverse proxy can reach it.
- **Key from env, not config files**: The API key is a single env var, never checked into git. `.env`, `.env.meme-bot`, and `.env.api-server` are in `.gitignore`.

## Namecheap

`NamecheapRegistrar` calls the real Namecheap XML API:

- `namecheap.domains.check` — checks availability and rejects premium/EAP domains.
- `namecheap.users.getPricing` — reads the 1-year registration price.
- `namecheap.domains.create` — purchases the domain using unified contact fields.

Supported TLDs: `.com`, `.net`, `.org`, `.xyz`, `.fun`, `.site`, `.online`, `.io`, `.co`, `.me`, `.cc`, `.app`, `.dev`, `.world`, `.space`, `.club`, `.store`, `.shop`, `.vip`.

### Required settings

```bash
NAMECHEAP_API_USER=
NAMECHEAP_API_KEY=
NAMECHEAP_USERNAME=
NAMECHEAP_CLIENT_IP=
NAMECHEAP_SANDBOX=true
NAMECHEAP_CONTACT_FIRST_NAME=
NAMECHEAP_CONTACT_LAST_NAME=
NAMECHEAP_CONTACT_ADDRESS1=
NAMECHEAP_CONTACT_CITY=
NAMECHEAP_CONTACT_STATE=
NAMECHEAP_CONTACT_POSTAL_CODE=
NAMECHEAP_CONTACT_COUNTRY=US
NAMECHEAP_CONTACT_PHONE=+1.5555555555
NAMECHEAP_CONTACT_EMAIL=
NAMECHEAP_CONTACT_ORGANIZATION=
```

Production requires Namecheap API access enabled and the server IPv4 whitelisted in Namecheap. `NAMECHEAP_SANDBOX` defaults to `true`; set to `false` only when ready for real purchases.

## Cloudflare Pages (Git Integration)

The `POST /pages/deploy` endpoint creates or verifies a Cloudflare Pages project connected to a GitHub repository, triggers a deployment, waits for deployment success, and optionally binds the custom domain. This keeps Cloudflare credentials and deployment responsibility inside `api-server`; `meme-bot` only generates the site and pushes code to GitHub.

### Configuration

```bash
CLOUDFLARE_ACCOUNT_ID=
CLOUDFLARE_API_TOKEN=
GITHUB_OWNER=          # GitHub username/org that owns the repo
GITHUB_TOKEN=          # GitHub personal access token (repo scope)
```

The `GITHUB_TOKEN` is needed on the bot side to push generated site files to a GitHub repository. The API server then creates a Pages project pointing at that repo.

## Agent Generation

The generation worker runs Gemini, Codex, or Claude Code as an external CLI in non-interactive mode:

```bash
GEMINI_AGENT_COMMAND=gemini
GEMINI_AGENT_ARGS="--yolo --prompt-file {prompt_file}"
CODEX_AGENT_COMMAND=codex
CODEX_AGENT_ARGS="exec --dangerously-bypass-approvals-and-sandbox --cd {output_dir} --file {prompt_file}"
CLAUDE_AGENT_COMMAND=claude
CLAUDE_AGENT_ARGS="-p {prompt} --output-format stream-json --verbose --allowedTools Read Edit Write Bash --permission-mode plan --dangerously-bypass-sandbox"
AGENT_TIMEOUT_SECONDS=900
GENERATION_PRIMARY_PROVIDER=gemini
GENERATION_MAX_CONCURRENCY=2
GENERATED_SITES_DIR=/www/websites
```

`GENERATION_PRIMARY_PROVIDER` sets the first provider to try on each job. Supported values: `gemini` (default), `codex`, `claude`. The router falls back through the other two providers on recoverable errors (quota exhausted, rate limited, provider unavailable).

For a paid order using `aaabbb.com`, the worker creates `/www/websites/aaabbb.com`, asks the selected agent to write the static site there, creates/pushes the GitHub repository named `aaabbb.com`, then asks `api-server` to deploy that repo through Cloudflare Pages and bind `aaabbb.com`.

The app considers the agent finished when:
- the CLI exits successfully
- `.agent_done.json` exists with `"status": "completed"`
- the output directory contains `index.html`, `package.json`, or `dist/index.html`

## Telegram Bot Setup

The Telegram bot runs as part of the `meme-bot` binary. It requires:

```bash
# Required
TELEGRAM_BOT_TOKEN=<bot token>
DATABASE_URL=sqlite://./db.sqlite3?mode=rwc
RUST_LOG=info
TEMPLATE_CATALOG_URL=https://your-template-site.example
ADMIN_CONTACT=@your_admin

# Optional, if api.telegram.org is unreachable:
TELOXIDE_PROXY=http://127.0.0.1:7890
```

The bot uses a 7-step inline keyboard wizard to collect order details, persists orders to PostgreSQL when the user selects a payment chain, and enqueues generation jobs when the TX hash is submitted. The queue worker picks up and processes jobs automatically.

## Test

```bash
cargo test
```

## Project Structure

```
mem-bot/
├── migrations/          # SQLx migration files
├── src/
│   ├── lib.rs           # Library root (shared modules)
│   ├── bin/
│   │   ├── meme-bot.rs      # Telegram bot + queue worker
│   │   └── api-server.rs    # API server binary
│   ├── app.rs           # App wiring (queue + deployer)
│   ├── config.rs        # Env var configuration
│   ├── db.rs            # Database queries
│   ├── deployment.rs    # SiteDeployer trait + impls
│   ├── domain.rs        # DomainRegistrar/DnsProvisioner trait + impls
│   ├── generation.rs    # AI agent generation
│   ├── models.rs        # Shared domain types
│   ├── payment.rs       # Payment verification
│   ├── queue.rs         # Generation job queue
│   ├── telegram.rs      # Telegram bot UI
│   └── validation.rs    # Input validation
├── templates/           # Local template files
├── .cargo/
│   └── config.toml      # Cargo aliases (build-musl-release)
├── Cargo.toml
└── CLAUDE.md
```
