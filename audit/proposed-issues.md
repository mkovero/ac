Read audit. 8 recommended issues + epic candidate. List below. Nothing created.

**Issues I would create:**

1. **Single-source ZMQ frame types in `ac-core` + add producer↔consumer round-trip test**
   - category: `infrastructure`
   - labels: `infrastructure`, `needs-design`, `agent:triage`
   - rationale: Highest blast-radius gap; wire schema triple-defined, touches ZMQ protocol → affects `ds`+ui, needs architect on type hoist.

2. **Refresh architecture map + correct H1/LKFS invariants in specs**
   - category: `docs`
   - labels: `docs`, `ready-to-implement`, `agent:triage`
   - rationale: Stale module map + misstated invariants mislead devs; pure doc fix, no output surface.

3. **Fix THD+N denominator to total signal level + add high-distortion test**
   - category: `measurement-accuracy`
   - labels: `measurement-accuracy`, `needs-design`, `agent:triage`
   - rationale: Standards deviation (AES17 §6.3.1) — fix-vs-document-deviation decision needs architect; currently invisible to tests.

4. **THD+N display: show `dB re fund` alongside `%` with reference label**
   - category: `feature`
   - labels: `feature`, `needs-ux`, `agent:triage`
   - rationale: Output-format change to CLI summary; depends on #3 deciding correct value. `needs-ux` mandatory (CLI output).

5. **CLI display rework: surface noise floor + duration, drop decorative borders, fix register**
   - category: `feature`
   - labels: `feature`, `needs-ux`, `agent:triage`
   - rationale: Display-only, daemon already emits data; touches `ac` stdout → `needs-ux` required.

6. **EPIC: ds drop it**
   - category: `infrastructure`
   - labels: `infrastructure`, `epic`, `agent:triage`
   - rationale: obsolete, will do something else in future if interested. remove related files and folders

7. **Add BS.468-4 Table-2 QP burst-response tests (or downgrade citation)**
   - category: `measurement-accuracy`
   - labels: `measurement-accuracy`, `needs-design`, `agent:triage`
   - rationale: Last untestable measurement-correctness gap; "test vs downgrade" is a scope decision → architect.

8. **Test-hygiene sweep: dead `thd.rs:308`, `filterbank.rs:342` bpo-dependence, `weighting.rs` 31.0→31.5 Hz, filterbank citation**
   - category: `infrastructure`
   - labels: `infrastructure`, `ready-to-implement`, `agent:triage`
   - rationale: Small mechanical fixes removing false test confidence; well-scoped.

**Notes:**
- #4, #5, #6c all hit CLI/output → `needs-ux` per standing `ac` output requirement.
- #1, #3, #6a, #7 carry protocol/standards/contract decisions → `needs-design`.
- #6 marked `epic` (3 separable work units, ordering dependency on filename cutover).
- Sequence per audit: foundational-first (1→8). #4 blocked-on #3, #6c blocked-on #6a/#6b.
