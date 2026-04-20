# Refactor notes

Active record of decisions and unresolved items during the phased `ac` Rust
refactor. Delete when empty.

## Baseline (HEAD = 9beafbd)

Tests (verified 2026-04-21):

| Crate        | Tests          |
|--------------|----------------|
| ac-cli       | 50             |
| ac-core      | 71             |
| ac-daemon    | 43 unit + 10 it |
| ac-ui        | 100            |
| **Total**    | **274**        |
| pytest tests/ | 22 passed (~3:30) |

Baseline output saved to `$TMPDIR/baseline-tests.txt` and
`$TMPDIR/baseline-clippy-full.txt` (ephemeral; not committed).

`ac-rs/CLAUDE.md` reports 245 (49+50+43+93). Stale — update in Phase 4.
Root `CLAUDE.md` reports 227. Stale — update in Phase 4.

### Pre-existing clippy errors (NOT caused by refactor)

`cargo clippy --workspace -- -D warnings` fails on HEAD with **10 errors**:

| File | Lines | Lint |
|------|-------|------|
| `ac-ui/build.rs` | 16, 35, 59, 64, 66 | complex type, too many args, 3x excessive float precision |
| `ac-core/src/aggregate.rs` | 35 (3x), 101 (2x) | `neg_cmp_op_on_partial_ord` |

Strategy: I will NOT fix these (out of scope — anti-goal "While I was in there
I also…"). Instead, I will diff clippy output against this baseline after each
phase to ensure I introduce no NEW warnings. Treat the above 10 as the
allowed-error set.

## Dead-code audit (Phase 0)

Audit method: per file, remove all `#[allow(dead_code)]` markers, run
`cargo check -p ac-ui`, restore only those that trigger warnings, add grouped
reason comments.

Total 12 marker sites (one was a duplicate field-level annotation). Outcome:

| Site | Status | Reason |
|------|--------|--------|
| `render/context.rs` `instance`, `adapter` | Retained + comment | wgpu resources held for Drop ordering |
| `data/types.rs` `SpectrumFrame.n_channels` | Retained + comment | wire-protocol field; no UI consumer |
| `data/types.rs` `CwtFrame.n_channels` | **Deleted** | Actually read by receiver.rs:241; marker was unnecessary |
| `data/types.rs` `FrameMeta.{freq_hz, fundamental_dbfs, thd_pct, thdn_pct, xruns}` | Retained (existing comment expanded) | daemon `analyze()` output kept for future per-frame inspection |
| `data/types.rs` `TunerFrame.{baseline_db, range_lock, timestamp}` | Retained + comment | diagnostics fields; no UI consumer |
| `data/store.rs` `VirtualChannelStore::store_for`, `clear` | Retained + comment | exercised by unit tests, no production caller |
| `data/store.rs` `TransferRegistry::clear` | Retained + comment | exercised by unit tests, no production caller |
| `app.rs` `DataSource::Synthetic` tuple field | Retained + comment | handle owns worker thread; Drop stops it |

One marker deleted (CwtFrame.n_channels). Eleven retained with explanatory
comments. No production behaviour changed.

## Open items

_(things to revisit)_

## Decisions that may need revisiting

_(things the plan constrained but may warrant follow-up later)_
