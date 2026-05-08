# Repository Guidelines

## Project Structure & Module Organization

This is a Rust 2024 crate with two binaries and one shared library. Binary entry points live in `src/bin/`: `meme-bot.rs` runs the Telegram bot plus queue worker, and `api-server.rs` exposes REST endpoints for Namecheap and Cloudflare operations. Shared modules live directly under `src/`: `config`, `db`, `telegram`, `queue`, `generation`, `deployment`, `domain`, `payment`, `validation`, `models`, and `app`. Database schema changes belong in `migrations/`; the current SQLite schema starts at `migrations/0001_init.sql`. Configuration examples are in `.env.example`.

## Build, Test, and Development Commands

- `cargo build` compiles all targets in debug mode.
- `cargo run` runs the default `meme-bot` binary.
- `cargo run --bin api-server` starts the API service.
- `cargo test` runs all unit and async tests.
- `cargo fmt` formats Rust code using rustfmt.
- `cargo clippy --all-targets --all-features` checks for common Rust issues.
- `cargo build-musl-release` builds static Linux release binaries via the alias in `.cargo/config.toml`.

Before running services locally, create `.env` from `.env.example` and fill required secrets such as `TELEGRAM_BOT_TOKEN` and API provider credentials.

## Coding Style & Naming Conventions

Follow standard Rust style: 4-space indentation, `snake_case` for functions and modules, `PascalCase` for types and traits, and `SCREAMING_SNAKE_CASE` for constants. Keep external integrations behind traits such as `PaymentVerifier`, `SiteGenerationProvider`, `SiteDeployer`, `DomainRegistrar`, and `DnsProvisioner`. Prefer structured validation and typed models over ad hoc strings, especially for Telegram user input, payment chains, domains, and deployment metadata.

## Testing Guidelines

Tests are colocated with implementation modules under `#[cfg(test)] mod tests`, using `#[test]` and `#[tokio::test]`. Add tests near the code they exercise, following existing names like validation, payment, generation, deployment, and domain tests. Use stub or dry-run implementations for external services; do not require real Telegram, Namecheap, Cloudflare, or chain credentials in tests. Run `cargo test` before opening a PR.

## Commit & Pull Request Guidelines

Recent commits use short imperative summaries, for example `Migrate database from PostgreSQL to SQLite` and `Expand supported domain TLDs and add domain integration tests`. Keep commit subjects focused on the behavior changed. Pull requests should include a brief description, test results, any migration or env-var changes, and screenshots or API examples when UI flows or HTTP responses change.

## Security & Configuration Tips

Do not commit `.env`, real API keys, bot tokens, database files, or generated deployment artifacts. Keep new configuration as flat environment variables read through `Config::from_env()` and document additions in `.env.example`.
