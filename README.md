# Meme Bot

Rust backend scaffold for a Telegram bot that sells and launches meme websites.

## Implemented

- PostgreSQL schema managed by SQLx migrations.
- Core entities: `users`, `orders`, `websites`, `templates`, `payment_attempts`, `generation_jobs`, `domain_quotes`, deployment/domain support tables.
- Structured website input validation. Users do not provide prompts.
- USDT TX verification service boundary with the agreed rules: fixed chain addresses, 30 minute payment window, overpayment allowed, retryable TX submissions.
- Generation provider router with transparent Gemini/Codex failover.
- PostgreSQL-backed generation queue with global concurrency control.
- Namecheap, Cloudflare, GitHub/Git deployment boundaries with dry-run implementations.

## Run

1. Create a PostgreSQL database.
2. Copy `.env.example` to `.env` and set `DATABASE_URL`.
3. Run:

```bash
cargo run
```

The binary runs migrations and processes one generation queue tick. The next step is to turn `App::run_once` into the long-running Telegram bot plus worker loop.

## Test Telegram Frontend

For frontend-only testing, `.env` can omit `DATABASE_URL`:

```bash
TELEGRAM_FRONTEND_ONLY=true
TELEGRAM_BOT_TOKEN=<bot token>
TEMPLATE_CATALOG_URL=https://your-template-site.example
ADMIN_CONTACT=@your_admin
# Optional, but usually required if api.telegram.org times out:
TELOXIDE_PROXY=http://127.0.0.1:7890
```

Then run:

```bash
cargo run
```

The bot starts with `/start`, `/create`, `/status`, `/help`, inline buttons, template deep-link handling, and TX hash input handling. It does not write orders to PostgreSQL in frontend-only mode.

To run the website generation agent after a submitted TX in frontend test mode:

```bash
FRONTEND_RUN_AGENT_AFTER_TX=true
```

The payment check is still bypassed in frontend test mode, so use this only when you intentionally want Gemini/Codex to run.

## Test

```bash
cargo test
```

## Next Adapters To Replace

- `payment::StubPaymentVerifier`: replace with ERC20/TRON/SOL chain verifiers.
- `deployment::GitCliCloudflareDeployer`: add GitHub repo creation API and Cloudflare Pages project API before the git push/deploy path.
- `telegram`: replace message builders with real Telegram bot update handling.

## Namecheap

`domain::NamecheapRegistrar` calls the real Namecheap XML API:

- `namecheap.domains.check` checks availability and rejects premium/EAP domains for automatic flow.
- `namecheap.users.getPricing` reads the 1-year registration price.
- `namecheap.domains.create` purchases the domain using the unified contact fields in `.env`.

Required settings:

```bash
NAMECHEAP_API_USER=
NAMECHEAP_API_KEY=
NAMECHEAP_USERNAME=
NAMECHEAP_CLIENT_IP=
NAMECHEAP_SANDBOX=true
NAMECHEAP_AUTO_PURCHASE_AFTER_TX=false
NAMECHEAP_CONTACT_FIRST_NAME=
NAMECHEAP_CONTACT_LAST_NAME=
NAMECHEAP_CONTACT_ADDRESS1=
NAMECHEAP_CONTACT_CITY=
NAMECHEAP_CONTACT_STATE=
NAMECHEAP_CONTACT_POSTAL_CODE=
NAMECHEAP_CONTACT_COUNTRY=US
NAMECHEAP_CONTACT_PHONE=+1.5555555555
NAMECHEAP_CONTACT_EMAIL=
```

Production requires Namecheap API access enabled and the server IPv4 whitelisted in Namecheap.

`NAMECHEAP_AUTO_PURCHASE_AFTER_TX` defaults to `false` because the current Telegram frontend can bypass payment verification for testing. Set it to `true` only when you intentionally want the bot to call `domains.create` after a submitted TX.

## Agent Generation

The generation worker runs Gemini/Codex as an external CLI in non-interactive YOLO mode. Defaults:

```bash
GEMINI_AGENT_COMMAND=gemini
GEMINI_AGENT_ARGS=--yolo --prompt-file {prompt_file}
CODEX_AGENT_COMMAND=codex
CODEX_AGENT_ARGS=exec --dangerously-bypass-approvals-and-sandbox --cd {output_dir} --file {prompt_file}
AGENT_TIMEOUT_SECONDS=900
```

The app considers the agent finished only when:

- the CLI exits successfully
- `.agent_done.json` exists with `"status": "completed"`
- the output directory contains `index.html`, `package.json`, or `dist/index.html`

## Cloudflare Pages Deploy

The app can deploy generated sites to Cloudflare Pages through Direct Upload:

```bash
CLOUDFLARE_ACCOUNT_ID=
CLOUDFLARE_API_TOKEN=
CLOUDFLARE_WRANGLER_COMMAND=npx
CLOUDFLARE_WRANGLER_ARGS=wrangler pages deploy {site_dir} --project-name={project_name} --branch={branch}
```

Frontend test mode can deploy immediately after agent completion:

```bash
FRONTEND_RUN_AGENT_AFTER_TX=true
FRONTEND_DEPLOY_AFTER_AGENT=true
```

The deployer:

- ensures the Pages project exists through the Cloudflare API
- runs Wrangler Pages Direct Upload for the generated output directory
- binds the user's requested domain as the Pages custom domain

For the custom domain to become live, the domain must be on Cloudflare DNS or have the required DNS records configured.
