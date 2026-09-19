# Repository Ablation and Legacy Template Removal Design

## Status

Approved in conversation on 2026-09-19. This document defines the scope and
acceptance criteria for simplifying the repository. It does not authorize
behavioral redesign outside the boundaries below.

## Objective

Reduce the amount of code and indirection required to understand, operate, and
change the project while preserving its production behavior. The work has two
linked outcomes:

1. Completely remove the obsolete internal template catalog and automated
   template-order implementation, including its database schema.
2. Run evidence-based ablation experiments on the remaining repository and
   keep only changes that measurably simplify the code without weakening
   behavior, error reporting, tests, or security boundaries.

The public manual template offer is not the obsolete template subsystem. It
must remain: the Telegram UI advertises a fixed price of 200 USDT, links to
`https://memewebs.store`, and tells the customer to contact the configured
administrator. It must not create a template order inside the bot.

## Constraints

- Keep `meme-bot` and `api-server` as separate processes. The separation is a
  security boundary that keeps Namecheap and Cloudflare credentials out of the
  Telegram bot process.
- Preserve the REST API paths and externally observable behavior.
- Preserve AI website creation, payments, domain registration, deployment,
  website editing, and their stored history.
- Preserve traits at external integration boundaries and their test doubles:
  payment verification, site generation, deployment, domain registration, and
  DNS provisioning.
- Keep SQLite with a single pooled connection and the existing queue-locking
  model.
- Money remains represented by `rust_decimal::Decimal`; no money value may be
  converted to floating point.
- Do not alter `migrations/0001_init.sql`, because deployed databases have
  already recorded it. All schema removal belongs in a new migration.
- Breaking the Rust library's incidental public API is allowed. Only the two
  binaries, REST API behavior, Telegram behavior, and persistent data are
  compatibility requirements.

## Considered Approaches

### 1. Conservative cleanup

Delete only the known template code and obvious dead dependencies. This has
the lowest immediate risk but leaves multiple forwarding wrappers and an
oversized public surface without testing whether they provide value.

### 2. Staged, evidence-driven ablation (selected)

Establish behavior and complexity baselines, then remove one coherent layer at
a time. Run the complete verification suite and compare complexity after every
batch. Keep only batches that reduce complexity while preserving behavior.

This is selected because it makes aggressive cleanup reversible and produces
evidence for every retained deletion.

### 3. Radical flattening

Collapse most modules and traits into the binaries. This could minimize type
count but would mix unrelated responsibilities, weaken test seams, and expose
secrets to a larger process. It is rejected.

## Target Architecture

The final system retains these meaningful boundaries:

- `meme-bot`: Telegram updates, customer workflow, payment handling, and the
  generation queue worker.
- `api-server`: authenticated Namecheap and Cloudflare operations.
- database stores: centralized SQL and row mapping for users, orders, and
  websites.
- external-integration traits: stable seams around network, agent, deployment,
  registrar, and DNS providers.
- domain types that encode real state or policy, including order status,
  screens, and payment-transaction processing.

The following are not architectural boundaries and may be removed when their
call sites remain clearer afterward:

- one-field or one-call forwarding wrappers;
- zero-state namespace types containing only associated functions;
- configuration containers used at a single construction site;
- result fields no caller reads;
- exports used only because older code exposed them;
- dependencies with no compiled use.

Large modules are not to be flattened merely to reduce the number of types.
The goal is lower conceptual and call-path complexity, not the smallest raw
file count.

## Legacy Template Subsystem Removal

Remove all internal catalog and automated-order artifacts:

- the `Template` model and `TemplateStore`;
- startup catalog synchronization;
- `BuildMode`, `Order.build_mode`, and `Order.template_id`;
- template fields in site-generation requests;
- internal template pricing, preview, and local-template configuration;
- old template checkout callbacks, deep-link parsing, and tests that exist only
  for the retired checkout;
- obsolete README schema/configuration documentation and environment examples;
- public exports and dependencies left unused by this removal.

Keep the current manual template contact screen, its fixed 200 USDT price,
`memewebs.store` link, administrator contact, and tests for those facts.
AI-order creation becomes the only order-creation path, so its APIs no longer
accept a build mode or template identifier.

Callbacks and deep links embedded in messages from the retired checkout are
explicitly unsupported after this change. The current `template_mode` entry
continues to open the manual contact screen.

The word "template" used for agent prompt templates or ordinary string
templates is unrelated and must not be removed.

## Database Migration

Add `migrations/0002_remove_template_system.sql`. A fresh database must run
`0001` followed by `0002`; an existing database must run only the new pending
migration.

Before removing columns, migrate legacy order state as follows:

- Orders with `build_mode = 'template'` and status other than `done` or
  `cancelled` become `need_manual_review`.
- Those transitioned orders have `locked_by` and `locked_at` cleared so a
  queue worker cannot continue obsolete automated work.
- Completed and cancelled template orders retain their existing status.
- AI orders retain their existing status.
- All remaining order fields, including monetary values, payment hashes,
  timestamps, edit state, retry counts, and collected website inputs, are
  copied without transformation.
- All user and website rows are preserved. A completed template website must
  continue to reference its historical order.

Rebuild `orders` without `build_mode` and `template_id`, preserving every
other column definition and check constraint. Recreate all order indexes.
Because `websites.order_id` references `orders`, rebuild `websites` in the same
transaction with its existing schema and unchanged row values. Create and
populate replacement tables first, drop the original `websites` before the
original `orders`, rename the replacements, and then recreate indexes. This
ordering preserves foreign-key validity without disabling foreign-key
enforcement. Drop `templates` only after no retained schema references it.

The migration must be atomic. On failure, the original schema and data must
remain usable. After migration, `PRAGMA foreign_key_check` must return no rows.
The test must also inspect `sqlite_schema`/`PRAGMA table_info` to prove that the
`templates` table and the two removed order columns no longer exist.

This schema migration is intentionally forward-only. Deployment requires a
copy of the SQLite database before starting either new binary. Operational
rollback means stopping the new binaries, restoring that backup, and starting
the old binaries. Old binaries must not be run against the migrated database.

## Ablation Batches

### Batch 1: Retire the internal template subsystem

Add migration characterization coverage, apply the migration, simplify the
models and stores, remove catalog startup/configuration, and reduce AI order
creation and generation requests to their only supported shape. Update user
documentation and configuration examples in the same batch.

### Batch 2: Remove forwarding abstractions

Evaluate these candidates independently:

- inline `App` if it only stores configuration/pool and forwards queue
  construction;
- inline `GenerationQueueConfig` if its only role is moving existing fields
  into one call;
- replace the zero-state `Keyboards` namespace with module functions;
- replace the one-field `DomainFlow` wrapper with focused functions operating
  on `&dyn DomainRegistrar`, while preserving retries and alternative-domain
  checks;
- remove unread result fields such as `SiteGenerationResult.output_dir`;
- make `Config.database_url` a `String` if construction proves it always has a
  value.

Each candidate is retained only if the resulting call sites are at least as
clear and one of the defined complexity measures decreases. Candidates are
not all-or-nothing; a failed candidate is reverted without blocking the rest
of the batch.

### Batch 3: Reduce dependency and visibility surface

- Remove direct dependencies with no source or test use, with `tower` as the
  first candidate.
- Change symbols to the narrowest visibility that still supports both binary
  crates and colocated tests.
- Remove dead re-exports, methods, result fields, and test helpers.
- Retain database stores and external-integration traits even when they have a
  single production implementation, because they centralize SQL or isolate an
  external boundary.

## Experiment Protocol

Record a baseline at the implementation starting commit and repeat the same
measurements after every batch:

- Rust and migration source lines;
- number of declared structs, enums, and traits;
- number of public modules, types, functions, traits, and re-exports;
- number of direct Cargo dependencies;
- call depth for each wrapper candidate, documented from its primary call
  site;
- complete test count and result.

An ablation is accepted only when:

1. at least one applicable complexity measure decreases;
2. no applicable complexity measure grows without a documented reason;
3. public behavior and persisted data remain correct;
4. error context and recoverability are not weakened;
5. the verification suite passes.

If deletion requires more replacement code or a new abstraction of equivalent
complexity, revert it. Keep each accepted batch in a separate logical commit so
it can be reviewed or reverted independently.

## Error Handling

Continue using `anyhow::Result` with contextual errors for orchestration and
`thiserror` enums for domain failures. Do not replace propagated errors with
`unwrap`, silent defaults, logging-only failure, or discarded provider
details. Preserve retry policy and recoverable/non-recoverable classification
at external boundaries.

Database migration errors must identify the failed migration and prevent
either binary from starting. A legacy order moved to manual review is a valid
business state, not an error.

## Testing Strategy

Add a migration integration test that creates a database at the `0001` schema,
seeds at least:

- one AI order;
- one unfinished template order with queue locks;
- one completed template order;
- one cancelled template order;
- payment, amount, edit-state, and retry values that can detect lossy copying;
- a website referencing the completed template order.

Run `0002` and assert the state rules, value preservation, schema removal,
index recreation, and a clean foreign-key check. The test must run without
network access or credentials.

Before deleting or inlining a non-template abstraction, use existing tests or
add focused characterization tests for behavior that is not otherwise pinned
down. Do not preserve tests that merely assert the shape of the abstraction
being deleted.

Every accepted batch must pass:

```text
cargo fmt --check
cargo test
cargo clippy --all-targets --all-features -- -D warnings
cargo build --bin meme-bot
cargo build --bin api-server
```

## Acceptance Criteria

- The manual template screen displays 200 USDT, `memewebs.store`, and the
  configured administrator contact, and cannot create an in-bot template
  order.
- No internal template catalog model, store, synchronization, generation
  field, configuration, table, or order column remains.
- Legacy unfinished template orders are in manual review with no queue lock;
  completed/cancelled order history and all websites remain intact.
- AI creation, payments, domains, deployment, editing, Telegram flows, and
  REST API behavior continue to pass their tests.
- `meme-bot` and `api-server` remain separate deployable binaries.
- External integrations remain behind traits with credential-free test
  doubles.
- Formatting, all tests, strict Clippy, and both binary builds pass.
- The final report includes before/after complexity measurements and identifies
  any proposed ablation that was reverted.
- The repository has a net reduction in measured complexity relative to the
  implementation starting commit.
