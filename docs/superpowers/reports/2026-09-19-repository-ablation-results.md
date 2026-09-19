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

## Deployment

Before deploying migration 0002, stop both binaries and copy `db.sqlite3` to
`db.sqlite3.pre-0002`. Rollback requires restoring that copy before starting
the old binaries.
