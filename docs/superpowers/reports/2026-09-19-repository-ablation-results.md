# Repository Ablation Results

Implementation baseline: `0cd5e68`

## Fixed Measurement Commands

The implementation plan contains the exact commands. Every batch uses those
commands unchanged so the measurements remain comparable.

## Baseline

| Metric | Baseline |
|---|---:|
| Rust and migration lines | 11,623 |
| Structs, enums, and traits | 78 |
| Public symbols | 200 |
| Direct dependencies | 23 |
| Listed tests | 65 |

## Wrapper Candidates

- Queue construction: `main -> App::build_queue -> GenerationQueue::new`; `App::run_once` has no caller.
- `GenerationQueueConfig`: one construction site and one consumer.
- `Keyboards`: zero stored state; all methods are namespace functions.
- `DomainFlow`: one `&dyn DomainRegistrar` field; two behavior methods.
- `SiteGenerationResult.output_dir`: written by generation and never read.
- `Config.database_url`: always receives either `DATABASE_URL` or a default, but is stored as `Option<String>`.

## Batch Results

Results are appended only after a batch passes its required checks. A rejected
candidate is recorded with the reason it was reverted.

### Batch 1 — legacy template removal

| Metric | Observed | Change from baseline |
|---|---:|---:|
| Rust and migration lines | 11,645 | +22 |
| Structs, enums, and traits | 75 | -3 |
| Public symbols | 192 | -8 |
| Direct dependencies | 23 | 0 |
| Listed tests | 62 | -3 |

Verification: migration preservation and rollback tests pass; the complete test
suite passes with 55 passed and 7 ignored; strict Clippy and both binary builds
pass. The intentional line increase comes from explicit migration preservation
coverage. Retained: yes.

### Batch 2 — forwarding abstractions

| Metric | Observed | Change from Batch 1 |
|---|---:|---:|
| Rust and migration lines | 11,600 | -45 |
| Structs, enums, and traits | 73 | -2 |
| Public symbols | 187 | -5 |
| Direct dependencies | 23 | 0 |
| Listed tests | 62 | 0 |

Removed `App` and `GenerationQueueConfig`. Queue construction now has one fewer
forwarding call layer: `main` calls `GenerationQueue::new` directly. The full
test suite and the `meme-bot` build pass. Retained: yes.

## Deployment

Before deploying migration 0002, stop both binaries and copy `db.sqlite3` to
`db.sqlite3.pre-0002`. Rollback requires restoring that copy before starting
the old binaries.
