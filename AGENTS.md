# Repository Guidelines

## Project Structure

Rust 2024 crate with two binaries and one shared library.

| Binary | Entry point | Role |
|--------|-------------|------|
| `meme-bot` | `src/bin/meme-bot.rs` | Telegram bot + queue worker (default `cargo run` target) |
| `api-server` | `src/bin/api-server.rs` | REST API for Namecheap/Cloudflare operations |

Shared modules live under `src/`: `config`, `db`, `telegram`, `queue`, `generation`, `deployment`, `domain`, `payment`, `validation`, `models`, `app`, `api_client`, `logging`. Database schema: `migrations/0001_init.sql`. AI agent prompts: `prompts/agent-{gemini,codex,claude}.md`. Config examples: `.env.meme-bot.example` and `.env.api-server.example`.

## Build, Test, and Development Commands

```bash
cargo build                        # debug build
cargo run                          # runs meme-bot
cargo run --bin api-server         # start API server
cargo test                         # all tests (run before PRs)
cargo fmt                          # format code
cargo clippy                       # lint
cargo build-musl-release           # static Linux binaries (.cargo/config.toml alias)
```

Setup: copy `.env.meme-bot.example` → `.env.meme-bot` and `.env.api-server.example` → `.env.api-server`, then fill in real values. Override env file paths with `MEME_BOT_ENV_FILE` / `API_SERVER_ENV_FILE`. Set `RUST_LOG=debug` for verbose logging.

## Coding Conventions

### Error Handling

- **`anyhow::Result<T>`** for general error propagation. Use `.with_context(|| format!(...))` to add context.
- **`thiserror`** for domain-specific error enums (e.g., `PaymentVerificationError`, `GenerationError`). Derive `Debug, Error` and annotate variants with `#[error("...")]`.
- Classify errors as recoverable vs non-recoverable via methods like `is_provider_recoverable()`.

### Traits for External Boundaries

Every external integration is behind a trait with a real impl and a stub/dry-run impl for tests:

| Trait | Module | Test stub |
|-------|--------|-----------|
| `PaymentVerifier` | `payment` | inline test struct |
| `SiteGenerationProvider` | `generation` | `StaticTemplateProvider` |
| `SiteDeployer` | `deployment` | — |
| `DomainRegistrar` | `domain` | `HttpClientDomainRegistrar` |
| `DnsProvisioner` | `domain` | `DryRunDnsProvisioner` |

When adding a new external integration, define a trait, implement it, and provide a stub for tests.

### Database Conventions

- **SQLite** with `sqlx::SqlitePool`, **single connection** (`max_connections(1)`).
- `lock_next_job()` in `queue` uses a `tokio::sync::Mutex`-guarded SELECT+UPDATE for job locking.
- **IDs**: `Uuid::new_v4()` stored as TEXT in SQLite.
- **Amounts**: `rust_decimal::Decimal` — never use `f64` for money.
- **Timestamps**: `chrono::DateTime<Utc>`.
- Schema changes go in `migrations/`. Both binaries auto-run migrations via `sqlx::migrate!()`.

### Async Patterns

- `tokio::spawn` for background tasks; `tokio::select!` for concurrent task coordination.
- `Arc<Semaphore>` for concurrency limits (generation jobs).
- `Arc<Mutex<T>>` for shared mutable state (job lock, log writer).

### Configuration

All config is flat env vars loaded via `Config::from_env()`. No config files. When adding a new setting:
1. Add the field to `Config` in `src/config.rs`
2. Read it from `env::var(...)` in `Config::from_env()`
3. Document it in the matching `.env.*.example` file

### Naming

Standard Rust: `snake_case` functions/modules, `PascalCase` types/traits, `SCREAMING_SNAKE_CASE` constants.

## Testing Guidelines

- Tests are colocated: `#[cfg(test)] mod tests` inside each module.
- Sync tests: `#[test]`. Async tests: `#[tokio::test]`.
- Use stub/dry-run trait impls — never require real API credentials.
- `tempfile` crate for temporary directories in tests.
- Run `cargo test` before opening a PR.

## Commit Style

Short imperative subjects focused on behavior changed (e.g., `Add SOL payment chain support`). Include migration/env-var changes in PR description.

## Security

Do not commit `.env*` files (except `.example`), API keys, bot tokens, database files, or generated site artifacts. See `.gitignore`.
