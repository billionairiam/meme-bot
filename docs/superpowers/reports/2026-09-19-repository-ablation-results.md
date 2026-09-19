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

The keyboard namespace candidate also passed: replacing `Keyboards` with module
functions reduced the batch totals to 11,595 lines, 72 declared types, and 186
public symbols, with dependency and test counts unchanged. All keyboard,
screen, and full-suite tests pass. Retained: yes.

The domain-flow wrapper candidate passed with 11,701 lines, 72 declared types,
184 public symbols, 23 direct dependencies, and 65 listed tests. The raw type
count stayed flat because the test registrar stub replaced the deleted
production `DomainFlow` type; the line increase is the three new retry/error/
alternative characterization tests. Handler code now calls the two module
functions directly, and all 58 runnable tests pass with 7 ignored. Retained:
yes.

Removing the unread `SiteGenerationResult.output_dir` field reduced the total
to 11,699 lines while leaving type, public-symbol, dependency, and test counts
unchanged. Generation, queue, and full-suite tests pass. Retained: yes.

Making `Config.database_url` non-optional removed the impossible startup branch
and reduced the total to 11,694 lines. Other fixed metrics are unchanged; the
full suite and `meme-bot` build pass. Retained: yes.

### Batch 3 — dependency and visibility surface

| Metric | Observed | Change from prior batch |
|---|---:|---:|
| Rust and migration lines | 11,654 | -40 |
| Structs, enums, and traits | 72 | 0 |
| Public symbols | 183 | -1 |
| Direct dependencies | 22 | -1 |
| Listed tests | 65 | 0 |

Removed the direct `tower` dependency and the confirmed-dead
`optional_uuid_from_row` and `domain_error_for_user` functions. `tower`
remains available transitively where Axum needs it. Private root visibility for
`payment` and `validation` was rejected because strict Clippy correctly exposed
their otherwise external-facing APIs as dead; both public roots were restored.
The complete suite, strict Clippy, and both binary builds pass. Retained: yes.

## Deployment

Before deploying migration 0002, stop both binaries and copy `db.sqlite3` to
`db.sqlite3.pre-0002`. Rollback requires restoring that copy before starting
the old binaries.
