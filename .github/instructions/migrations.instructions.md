---
description: "Use when writing SQLite migrations, modifying database schema, or adding tables/columns in migrations/. Covers SQLite-specific constraints and project conventions."
applyTo: "migrations/**"
---
# Migration Conventions

- Single schema file pattern: add changes to new migration files (`migrations/0002_*.sql`, etc.)
- Both binaries auto-run migrations on startup via `sqlx::migrate!()`
- Enable foreign keys: `PRAGMA foreign_keys = ON;`
- IDs are `TEXT NOT NULL` storing UUID v4 values
- Amounts use `TEXT NOT NULL` for `rust_decimal::Decimal` (never REAL/float)
- Timestamps are `TEXT NOT NULL` storing ISO 8601 / `chrono::DateTime<Utc>`
- Status fields use `TEXT NOT NULL` with `CHECK(status IN (...))` constraints
- Add indexes for columns used in WHERE/ORDER BY queries (see existing `_idx` naming)
- Reference existing tables in [migrations/0001_init.sql](../../migrations/0001_init.sql) for naming patterns
