# Review Remediation Wave 1 — `eg` v1.0.6

- Author: Kiến trúc trưởng (spec-driven-design)
- Status: In Review (Round 6)
- Date: 2026-11-14
- Revision: r6 — records the arbitration round: the Commit 9 coverage test entry corrected against the shipped diff (Round-6 Resolution Map B1, no code change), the Commit 4/5 file scopes extended to the hunks they actually ship (B2), and the seven further unmapped hunks mapped into Commit 4/7/15 with the three-touch-path wording correction (B3); resolution maps in §Round-2 through §Round-6, history in §Revision History.
- Input artifacts: [docs/baselines/2026-11-14-eg-v1-0-6-review.md](../baselines/2026-11-14-eg-v1-0-6-review.md) (the 28-finding review; commit 1 relocates the currently-untracked `docs/review.md` there and redacts workstation paths before tracking — see Commit 1 and §Round-2/§Round-3 Resolution Maps B20) and the per-finding reconciliation delivered with the remediation ask.

## Overview

The review audited `eg` v1.0.6 with five independent review streams (Correctness & Invariants, Concurrency & Performance, Reuse & Contracts, Mechanism & YAGNI, Security & UI/A11y) after a clean pre-flight gate (`cargo fmt`, `cargo clippy -D warnings`, `cargo test --locked` 578/578, `allowlist_check.sh`, `write_path_check.sh`, `hygiene_check.sh` — all PASS, review §1). The reconciliation pass then verified every finding against the source, confirmed 22 claims fully and 7 partially, and found that several proposed fix diffs target imagined APIs or invert documented contracts.

This spec converts that reconciliation into an owner-approvable remediation plan: 26 findings fix now with corrected mechanisms, 2 deferred with reasons, 0 rejected, in 24 atomic commits. All fixes preserve the repository invariants: `.engram/`/`~/.engram/` write confinement (runtime write primitives stay in `db.rs`, `doctor.rs`, `journal.rs`, `build.rs`; new test fixtures go through the existing `crate::db::test_support` helpers — `src/db.rs:376-424`), zero outbound network, zero `unsafe`, MSRV 1.90 / edition 2021, no new dependencies, the 500-LOC-per-file hygiene gate (`tests/scripts/hygiene_check.sh:79-95`, `MAX_LOC=500` at `:80`, hard fail at `:94-95`), zero private leakage per `AGENTS.md §6` (the relocated review artifact is redacted before tracking — Commit 1), and CLI/MCP/docs synchronization per `AGENTS.md §5` (CHANGELOG included).

## The Problem

Four defect clusters ship in v1.0.6 despite the green gate:

1. **Silent data-quality failures.** `eg audit` computes `clipped` from `baseline_metrics.oldest_ts` (`src/audit/collect.rs:287-288`), which is always `> b_start` for entries admitted into the baseline bucket (`collect.rs:270-272`), so `baseline.sufficient` is always `false` and every baseline regression comparison is dead. `eg usage` prices `exit_code` 1 and 2 into `saved_tokens` (`src/journal/render.rs` aggregation vs the dashboard's Ok-only accounting in `src/dashboard/data/summary.rs`). `eg reuse` runs `eg-judge` (up to 5 subprocesses) and the freshness walk before discarding sub-threshold candidates (`src/query/reuse.rs:293-307`, `finalize.rs:31-38`).
2. **Memory lifecycle lock-in and tokenizer drift.** `current_revision` ignores `superseded_by`/`expires_at` (`src/db/memory_write.rs:317-331`): a cross-key superseded or expired key reports `Duplicate`/`Touched` on identical re-write and never re-enters the FTS tables; `mem write --touch` overwrites `anchored_at` contrary to the Touch contract (`docs/architecture/memory-store.md:116-120`); `mem touch` on a dead key still reports success (`memory_write.rs:416-431`). Separately, `mem_tokens_unicode` splits Vietnamese NFD text at combining marks (`tokens.rs:104-155`, `is_alphanumeric` is false for `Mn`), so NFD bodies tokenize into broken syllable pairs — and because the tokenizer contract version `MEM_FTS_EPOCH` stays `"2"` (`memory_fts.rs:8`), notes already indexed with broken tokens are never rebuilt.
3. **Cross-surface drift.** MCP `reuse` defaults `min_confidence` to `""` (`src/service/args.rs:257`), MCP `mem_query` emits error prefixes as `mem_query` while CLI says `mem query` (`args.rs:314-315`), `docs/health.md:115` documents a nonexistent `--key` flag, `docs/cli-reference.md:396` undercounts `--all-tools`, `README.md:155` and the `mem` help text in `src/cli.rs:230-231` omit `touch`/`reindex`, and `Cargo.toml` include omits `/tests/fixtures/*.rs` + `/docs/*.md` so `cargo test --locked` fails from a published crate.
4. **Dashboard/monitor truthfulness and hardening.** Click-to-filter on the Memory Store table fabricates a ghost `<option>` and empties the note list (`app.js:912-931` vs `populateMemoryStoreFilter` at `app.js:1325-1348`); expired notes report `is_current: true` (`memories.rs:347-350`); worktree rows collapse on every SSE render and agent tables lack keyboard access (`app.js:518-607`, `764-790`, `881-909`); `/tmp` stores leak into `/api/memory/touch` (`touch.rs:54`) and onto the `eg monitor` screen (`monitor/mod.rs:236-237`, `305-309` — no `entry_is_ephemeral` gate on either ingest loop); HEAD `/events` sends `Access-Control-Allow-Origin: *` (`server.rs:426`); `host_allowed` rejects `[::1]:port` (`http.rs:83-93`); the `.git` worktree pointer is layout-checked before canonicalization (`repo.rs:117-136`); journal line reads and dashboard threads are unbounded (`data.rs:165-184`, `301`, `server.rs:102-119`); agent names pass control characters into the journal and the raw-mode TUI (`agent.rs:60-70` — trim + 32-char cap only; `monitor/render.rs:421-426`).

## Goals

- Restore every contract named in `docs/architecture/memory-store.md`, `docs/architecture/code-index.md`, `docs/health.md`, and `AGENTS.md §3` that the 26 fix-now findings violate, with at least one regression assertion per restored contract.
- Every fix lands as an atomic, buildable, test-green commit grouped by file cluster, in the dependency order of §Detailed Implementation Plan, and every file stays at or below 500 LOC (`hygiene_check.sh` gate) via the per-file LOC budget in §Technical Plan.
- Zero behavior change beyond the enumerated findings; no drive-by refactoring (S-1 module splits excluded, see Non-Goals).
- Final pre-merge gate green: `cargo fmt --all -- --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test --locked`, `bash tests/scripts/allowlist_check.sh Cargo.lock`, `bash tests/scripts/write_path_check.sh`, `bash tests/scripts/hygiene_check.sh`.

## Non-Goals

- **S-1 function splitting** (9 functions over the 50-effective-line ceiling: `finalize::finalize` ~198 lines (`finalize.rs:7-205`), `proposal::analyze` ~140 (`proposal.rs:11-151`), `audit::collect::collect` (`collect.rs:207-340`), `handle_memory_touch` (`touch.rs:9-139`), `find_best_d1_deep` (`detectors/d1.rs:30-230`), `target::resolve_scope` 112 (`target.rs:11-123`), and peers). Behavior-neutral refactor; deferred to a dedicated wave. Only the helper deduplication half of S-1 ships now. Churn note (r3): the functions whose bodies this wave already touches are `collect::collect` (commits 2-3), `finalize::finalize` (commits 8-9, 23), `handle_memory_touch` (commit 15), and `analyze_corpus_with` (commit 8 inserts the threshold filter inside it, `reuse.rs:258-324`); the remaining three named functions (`proposal::analyze`, `find_best_d1_deep`, `target::resolve_scope`) have no body-touching commit — the split wave must not assume clean rebasing either way.
- **S-2 `kill_group` cost reduction.** The review-proposed skip-on-clean-exit contradicts the documented intent at `judge.rs:247-249` ("The judge may have exited after forking a helper that inherited stdout. Tear down the process group before draining so the read is bounded.") and would leak judge helpers and an undrained stdout pipe. Any safe variant needs measurement that 2 extra `/bin/kill` spawns per judge call matter; deferred.
- **S-6 dead `quick_check` branch.** `audit/memory.rs:101-104` rejects stores > 50 MiB before `inspect_store`, so the > 50 MiB `PRAGMA quick_check` arm at `audit/memory.rs:120-121` is unreachable. No written contract (grep over `docs/` finds no mention) mandates auditing large stores, so both "remove the collect() gate" and "delete the dead arm" are policy decisions, not remediation. Deferred for an owner ruling; zero runtime impact meanwhile.
- **Cross-store recall aggregation** in `aggregate_recalls` (`memories.rs:127-149` merges recall counts per key across stores): accepted limitation, advisory metric only.
- **`/tests/*.rs` include glob removal** in Cargo.toml: the glob matches nothing today and is harmless; left untouched to keep the diff minimal.
- **`docs/baselines/*` in the crate package**: the relocated review artifact is an internal audit baseline (hygiene-excluded per `tests/scripts/hygiene_check.sh:37`) and must not ship; commit 1 pins this negatively.
- Tail truncation of oversized journal generations (`render/read.rs:51-57`, `File::take(MAX_LOG_READ)` keeps the head) is not treated as baseline clipping; the sufficiency gate's in-window success/duration requirements remain the guard for that degradation.
- **MCP `initialize` root resolution** (`mcp.rs:91` still calls bare `discover_root`): outside the three sites review named for I-9; recorded in Open Questions as a follow-up candidate.

## Open Questions

- **S-6 policy**: should `eg audit` inspect memory stores above 50 MiB (remove the `collect()` gate so the existing `quick_check` arm runs) or is the 50 MiB ceiling final (delete the dead arm + `MemoryFailure::TooLarge`)? Owner decision due before the next remediation wave; not blocking this one.
- **S-1 module splits**: pick a wave after this one; see the churn note in Non-Goals for which functions this wave touches.
- **MCP `initialize` root parity** (`mcp.rs:91`): after commit 14, tool-call root inference goes through `RepoRef::resolve` while `initialize` keeps bare `discover_root` — a known divergence inherited from review scope; flag as a follow-up candidate rather than scope creep here.

## Critical User Journeys

### CUJ 1: Audit baseline regression works again (C-1, I-5)

- Given: a repository whose `.engram/usage.log` holds successful calls from before the baseline window start `b_start` (60 days ago) and within it, with `callers` p95 far above its critical latency in the recent window.
- When: `eg audit --json` runs.
- Then: `data.baseline.clipped` is `false` and `data.baseline.sufficient` is `true`; the `performance.p95` check reports `Status::Critical` (never downgraded to `Warn` by the 2x-baseline rule, which applies only when the static status is `Pass`).

### CUJ 2: A published crate builds and tests (C-2)

- Given: `cargo package` output of this crate (fixtures submodules `common/misc/python/rust/typescript` and the top-level `docs/*.md` included per the fixed include list; the internal review baseline under `docs/baselines/` excluded).
- When: `cargo test --locked` runs inside the packaged directory.
- Then: `tests/fixtures/main.rs` resolves all six module files (no `error[E0583]`), and `tests/cli/p8_impact.rs::documented_eg_commands_parse` reads `docs/` without `NotFound`; the new guard test pins the include entries as text, including the absence of `/docs/baselines/**`.

### CUJ 3: Superseded and expired keys unlock (I-1)

- Given: `k1` superseded by `k2` via `--supersedes`, or a session note whose `expires_at` passed.
- When: `eg mem touch k1` runs, then `eg mem write k1` with the same body as before supersession.
- Then: `mem touch` returns "not found" (exit 1 path), the re-write appends revision 2 and restores both FTS rows while the old dead row keeps its original `superseded_by` chain (it does not re-point to the new revision), and `mem get k1` returns the note. A `mem write --touch` on a live key refreshes `created_at` and `expires_at` and leaves `anchored_at` untouched.

### CUJ 4: Vietnamese NFD notes are findable (I-2)

- Given: a store whose bodies include both NFD and NFC spellings of Vietnamese words (the NFD form from macOS/PDF clipboards).
- When: commit 5 lands and the store is next opened for a write — `ensure_memory_fts_v2` detects the epoch change and rebuilds the Unicode index chunked (`memory_fts.rs:88-99`, triggered at `memory_store.rs:284-285`).
- Then: NFD and NFC bodies both tokenize to exactly one token per word (no syllable split), the rebuilt index matches NFC queries against NFD notes and NFD queries against NFD notes at the FTS5 layer (`remove_diacritics 2` folds both spellings to the same skeleton), and while a store's rebuild is pending reads degrade safely to the frozen legacy index (the `fts_v2_complete` marker at `src/db/memory_fts.rs:57-58` gates usability; the `ok_v2` flag flows into `crate::db::memory_fts_candidates` at `src/memory/guard.rs:106-108`; the legacy/v2 table choice happens at `src/db/memory_query.rs:125-129`, with the direct gate at `src/service/mem_ops.rs:137-141`).

### CUJ 5: Confidence gating stops paying for discarded candidates (I-3)

- Given: `--min-confidence medium` on a corpus whose raw pool is all Low, with `--judge` enabled and a counting judge stub installed via `EG_JUDGE_BIN`/`EG_JUDGE_MODEL` (the existing mechanism `tests/cli/reuse_phase5.rs` already exercises).
- When: `eg reuse` runs.
- Then: the judge stub's invocation counter file stays empty (zero judge subprocess spawns for sub-threshold candidates), freshness still walks the scope (so unindexed new files surface), and the counts line reports the sub-threshold total; the hint path (`for_hints`, which forces `min_confidence = Low`, `opts.rs:85`) is unaffected.

### CUJ 6: Allowlist suppression requires anchor currency (I-4)

- Given: a `reuse-allow/<target>-><canonical>` note whose `--refs` file is modified after the note's anchor time, then `eg build` runs; plus a second note referencing a file that has been moved away (its old path is gone from the index).
- When: `eg reuse` runs before and after `eg mem touch reuse-allow/<key>`.
- Then: before the touch the duplicate is reported (not suppressed); after the touch it is suppressed. The moved-file note never suppresses through a different file that merely suffix-matches the old path. Notes without `--refs` keep today's accepted behavior (never suppressed by the ref path), and symbol refs (no `/`) resolve through the symbol arm of the shared ref-status helper.

### CUJ 7: Dashboard memory panel tells the truth (I-13, I-14, I-15)

- Given: one local store and one expired session note, viewed in `eg dashboard`.
- When: the user clicks a Memory Store row, reads the KPI strip, and expands a worktree row, then waits one 3-second SSE cycle.
- Then: the click selects the `<option>` whose value equals the store id (no ghost option, no emptied list, second click returns to All by comparing the matched option value), the expired note reports `is_current: false` and is excluded from the Current filter, expanded worktree rows stay expanded across renders, the repository label remains a click-to-filter target on worktree rows (without also toggling the row), and both agent tables are operable with Tab + Enter/Space.

### CUJ 8: Monitor and server refuse hostile input (I-12, I-16, N-2)

- Given: a spoofed `.git` pointer `gitdir: /victim/.git/worktrees/..`, an agent name carrying ANSI escape bytes, a journal line longer than 256 KiB, and a request with `Host: [::1]:8080`.
- When: `worktree_main_root` resolves the pointer, the journal writer records the agent name, `eg monitor` renders the event, the dashboard polls the journal, and a browser hits the dashboard over bracketed IPv6 loopback.
- Then: the spoof resolves to `None` (canonicalized gitdir loses the `worktrees` segment), the agent name reaches the journal stripped of control characters, oversized lines are skipped at read time without materializing them, concurrent connections are capped with a logged 503 beyond the cap, and `[::1]:8080` is accepted while bare IPv6 Host forms and public hostnames stay rejected; `/api/memory/touch` and `eg monitor` in global mode list and show no `/tmp` store.

## Technical Plan

New source files are allowed only where the LOC gate demands them (§Round-2 Resolution Map B15): `src/dashboard/data/ingest.rs` (journal-line reading, entry validation, repo discovery, and tail — extracted from `data.rs` so `data.rs` returns under 500) and per-file regression-test submodules following the existing `#[cfg(test)] mod tests;` + sibling-file pattern already used by `src/journal/render/tests.rs` and the `src/db/*_tests.rs` family (`src/db.rs:447` declares `mod tests;`). No other production modules are added; both `SCHEMA_VERSION` and `MEMORY_SCHEMA_VERSION` are unchanged; the memory store's tokenizer contract version is the one deliberate exception handled by its designed migration lever (Commit 5).

Key shared decisions locked here (each derived from verified source, not the review diff):

1. **Baseline clipping (C-1)**: `clipped` becomes true exactly when the oldest valid journal entry across both generations is newer than `b_start` — i.e. the journal cannot prove coverage of the baseline window's start. `coverage.truncated` is deliberately excluded: truncation caps the tail (head kept, `render/read.rs:51-57`), and requiring `!truncated` would re-create the always-insufficient symptom for journals above the 16 MiB read cap. The review's proposed conjunct would report `clipped = false` exactly in the common 2-generation rotation case (rotation caps generations at 5 MiB, `ROTATE_BYTES`, `src/journal.rs:21`) — rejected.
2. **Touch TTL rule (I-1)**: re-assertion applies the current invocation's TTL policy — `mem write --touch` sets `created_at = now` and `expires_at` from the normalized input (explicit `--ttl` wins; a session note without `--ttl` gets the documented 7-day default per `memory.rs:132-134`); `anchored_at` is untouched on this path (contract at `memory-store.md:116-120`). Durable notes keep `expires_at = 0`. Consequence accepted and documented in the same commit (memory-store.md Write/Touch bullets, cli-reference `--touch` row, `SKILL.md` pillar-3, agent-integration `mem_write` row): re-asserting a session note without `--ttl` shortens a longer original TTL to 7 days — this is the OQ2 default policy for an un-parameterized session assertion.
3. **Supersede chain preservation (I-1, r2)**: when the current row is inactive, `try_write_revision` uses it only for revision numbering and skips `supersede_current` — the dead row keeps its original `superseded_by`/`superseded_at` pointing at whatever replaced it (`schema/memory.sql:28` semantics: `superseded_by` is the id of the row that superseded it). Re-stamping a cross-key-superseded row toward a same-key revision would corrupt the chain.
4. **Tokenizer contract migration (I-2, r2)**: the token change is an index-content change, not a query-shape change, so `MEM_FTS_EPOCH` bumps `"2"` → `"3"` in the same commit. The existing machinery (`ensure_memory_fts_v2`, `memory_fts.rs:88-99`) then drops and rebuilds `memories_fts_v2` chunked on the next write open per store (`memory_store.rs:284-285`), reads degrade to the frozen legacy table while a rebuild is pending (marker chain: `src/db/memory_fts.rs:57-58` → `src/memory/guard.rs:106-108` → `src/db/memory_query.rs:125-129` (direct legacy/v2 gate: `src/service/mem_ops.rs:137-141`)), and a reverted binary sees a stored epoch of `"3"` and rebuilds back under the old tokenizer — the migration is self-healing in both directions. `eg mem reindex` (`src/service/mem_reindex.rs:12` calls `ensure_memory_fts_v2(conn, true)`) remains the manual escape hatch.
5. **Allowlist ref freshness (I-4, r2/r3)**: `ref_is_fresh` branches on ref shape instead of keeping an ambiguous probe. Refs are consumed verbatim — no `path:line` splitting on either the suppression or the display face, so allowlist suppression and `mem query`/dashboard ref display always agree on the same string (the round-3 council point: a split used only by the allowlist would let a note suppress while the same ref still renders `[broken-ref]`). Path-shaped refs (the token contains `/`) require three things: the exact path exists in the `files` table, `!staleness::file_changed`, and the shared ref-status check returns `Valid` for the note's anchor. Non-path refs (no `/`) skip the files-table probe and go straight to the shared ref-status check's symbol arm. Consequences pinned: a moved file's old path fails the exact-existence term, so it can never suppress through a different file that merely suffix-matches (the shared helper's LIKE arm stays unreachable for path refs because the exact-match term is a precondition); symbol refs stop being rejected; ref-less notes keep today's accepted behavior (`allowlist.rs:36` gate unchanged — the review's `refs.is_empty() ||` inversion is rejected as an undocumented contract change).
6. **Ref-status helper lives in db (I-4 + r2 B16)**: `check_ref_status` and `RefStatus` move from `src/service/mem_query.rs` to `src/db/query_state.rs` as `pub(crate)` — `query/reuse` must not gain a first-ever edge to `service` (grep over `src/query/`, `src/db/`, `src/extract/`, `walk.rs` shows zero `crate::service` uses today; `service/mem_query.rs:402,406` already calls downward into `crate::query`). `mem_query.rs` re-points to the db layer; `allowlist.rs` consumes the db-layer helper.
7. **LIKE escaping (I-6 + S-1)**: one `pub(crate) like_escape` lives in `src/db.rs`; the private copies at `memories.rs:154-163`, `coverage.rs:160`, `target.rs:325` are deleted and their callers re-pointed. `column_present` consolidates on the `pragma_table_info`-based `Result<bool>` variant in `src/db/query_state.rs:83-94` promoted to `pub(crate)`; the two boolean copies wrap it with `.unwrap_or(false)` (identical semantics to today's swallow-and-false). Edge honesty (r2): `coverage.rs` and `target.rs` currently do not import `crate::db` — the dedup adds two new downward query→db edges; no cycle.
8. **FTS reconcile shape (I-10)**: the chunk selector splits the OR chain into a two-arm UNION ALL so arm 1 keeps `fts_version < 2` as an AND-ed term and can use the partial index `memories_pending` (`schema/memory.sql:37`, defined on `fts_version < 2` alone — not `superseded_by`, so both arms keep the `superseded_by IS NULL AND (expires_at = 0 OR expires_at > ?1)` predicates verbatim); each arm is bounded by the chunk limit inside a subselect, the outer query re-orders by `id` and limits to the chunk size. The five per-row statements become four hoisted prepared statements (fixed table names, no `format!` SQL) plus one `prepare_cached` UPDATE.
9. **Monitor filter parity and hygiene (I-16, r2)**: `TuiState::matches_filter` delegates to the linear-mode predicate; the unified predicate adopts the TUI's stricter repo-name handling (empty `repo_name` falls back to the basename of `repo`). Both monitor ingest loops (`mod.rs:236-237` TUI push, `mod.rs:305-309` linear render) gain an ephemeral exclusion gated on global-journal mode (the monitor is global exactly when `resolve_monitor_log_path` resolved no repo root), so `/tmp` scratch stores disappear from the monitor in global mode just as they do from every dashboard panel.
10. **Worktree row UX (I-15)**: a module-level `Set` keyed by repo display label persists expansion across renders; row click keeps toggling worktrees, and the repository label becomes its own focusable filter target (`role="button"`, `tabindex`, Enter/Space with `stopPropagation()` and `preventDefault()` so the row's toggle does not also fire); `aria-label` is removed from every `<tr role="row">` filter target across all four tables (five sites: 543/546, 657, 720, 835 — screen readers read the cells), and because no filter handler announces anything today, the four filter handlers gain one `announce('Filtered by <label>')` call each (Commit 18 item 4, pinned by the asset text-guard); agent rows in both tables get `tabindex` + Enter/Space mirroring `app.js:602, 605`. Label uniqueness is load-bearing: commit 22's disambiguated display names are what keep the expansion `Set` keys distinct.
11. **Connection and line bounds (I-12)**: journal readers switch to a capped line reader (cap `MAX_LINE_BYTES` 256 KiB, oversized lines skipped — the silent-skip contract is documented next to `data.rs:99`; discard is bounded by file length, and generations are small in practice because rotation caps them at 5 MiB while the read cap is 16 MiB via `render/read.rs:9`); the dashboard caps concurrent connections with an atomic counter + Drop guard (same pattern as `SSE_CLIENTS`/`SseGuard`, `server.rs:437-447`) and answers a logged 503 beyond the cap; the `.git` pointer read is capped at 4096 bytes and the gitdir is canonicalized before any lexical layout check; the initial HTTP request-line read gets the same header-byte cap the header loop already enforces.
12. **Audit tool taxonomy (I-8, r2)**: `canonical_tool` membership becomes `is_priced(tool) || crate::journal::UNPRICED_TOOLS.contains(&tool)` (24 names — verified: `is_priced` has 10, `UNPRICED_TOOLS` 14 including `audit` and `mem_reindex`, `tokens.rs:31-46`). `Metrics::add` routes `mem_export` into `mem_reads` (this restores parity with `summary.rs:45-49`) and `mem_reindex` into `mem_writes` on its own semantics (a reindex mutates the memory store's derived state). `classify`'s `mem_write` arm does not include `mem_reindex` today — that divergence is pre-existing, out of scope, and noted as such rather than "fixed" in a commit that must not touch the dashboard.

## Alternatives Considered

### Alternative A: Apply the review's drop-in diffs verbatim

- Pros: fastest; every diff arrives pre-written.
- Cons: verified inapplicable or contract-breaking in 14 of the 26 fix-now findings. Concrete evidence: C-1's conjunct re-enables insufficient baselines under rotation (Technical Plan §1); I-1's test calls nonexistent APIs (`memory_write_now`, `open_or_create_memory_db`) and an E0382 move; I-4 inverts the ref-less contract; I-8's ephemeral fix reads a field the writer never emits (`journal.rs:135-159` writes `worktree` only); I-9 references `resolve(None, Some(start))`/`into_root()` which do not exist (`repo.rs:38` takes one parameter); I-12 targets a nonexistent `detect_git_worktree`; I-14's patch does not apply to the real SQL and would shift the decode closure's column indexes; I-6 duplicates an existing private helper; S-2 leaks judge helper processes; S-3 references an undefined helper and a non-compiling iterator type change; C-1's own proposed test asserts `sufficient == true` for entries that begin at `b_start + 3600`, which the corrected clipping semantics necessarily reject — the diff and its test contradict each other.
- Reason for Rejection: violates the Zero-Decision Rule — an implementer following the diffs verbatim ships non-compiling or contract-breaking code.

### Alternative B: Fix only the findings whose diffs apply verbatim, defer the rest

- Pros: minimal review burden; smallest diff.
- Cons: leaves every Critical confirmed (C-1 baseline paralysis, C-2 broken published-crate tests) and 15 confirmed Important defects in place; the reconciliation already proved the corrected mechanisms compile and pass the existing suite for the riskiest cases (C-1 corrected formula run against the full suite: no failures).
- Reason for Rejection: the remediation ask is to restore the contracts the review documents as broken, not the subset that happens to be patch-ready.

### Alternative C (selected): Reconciliation-corrected fixes, deferred non-remediations

- Pros: every mechanism re-derived from the real source (verified line anchors in §Detailed Implementation Plan); contracts restored; two genuinely open policy calls (S-2, S-6) deferred instead of guessed; helper consolidation rides with the commits that need it; the round-1 self-contradictions the council caught (test-vs-mechanism pairs) are eliminated by construction in r2.
- Cons: 24 commits instead of one batch; implementers must follow this spec instead of the review's diffs.
- Reason for Selection: satisfies the Alternatives Rule with an evidenced losing pair, keeps every invariant, and converts each disputed diff into a named decision with source citations.

## Non-Functional Requirements

- **Security and privacy**: no new trust-boundary crossings; the hardening fixes (worktree pointer canonicalization, ANSI-safe agent names, bounded line and request-line reads, connection cap with a logged refusal, IPv6-correct Host check, ACAO removal) all shrink input surface. Zero outbound network, zero `unsafe`, runtime write primitives untouched — `write_path_check.sh` and `hygiene_check.sh` must stay green; new test fixtures go exclusively through `crate::db::test_support`.
- **Performance and latency**: FTS reconcile regains the `memories_pending` partial index (chunked, bounded); `class_candidates` prepares once per call instead of once per class; savings accounting loses the ~+100/+500/+1200-token inflation from exit-1/2 calls; one-time per-store FTS rebuild cost on first write-open after commit 5 (chunked, reads degrade to legacy meanwhile).
- **Rollout and rollback**: 24 atomic commits in dependency order, each green under targeted tests; rollback is per-commit `git revert` — no schema change, and the one contract change (FTS epoch) self-heals on revert (a reverted binary sees epoch `"3"`, rebuilds under the old tokenizer, `memory_fts.rs:88-99`). Docs/MCP/CLI contract changes land in the commits that change them (commit 4) plus one surface-sync commit (24) to keep every face synchronized per `AGENTS.md §5`, CHANGELOG included.

## Finding Disposition

Verdicts quote the reconciliation; corrections cite the source verified this session. Commit numbers refer to the r2 sequence (24 commits).

| ID | Sev | Claim (short) | Verdict | Decision | Correction vs review diff | Commit |
|---|---|---|---|---|---|---|
| C-1 | Critical | `clipped` always true, `baseline.sufficient` always false | confirmed | fixNow | Drop `coverage.truncated &&` — spec formula `coverage.oldest_ts > b_start` only; review's test data (entries starting at `b_start+3600`) contradicts the corrected semantics, tests redesigned incl. the `ts == b_start` boundary | 2 |
| C-2 | Critical | Cargo include missing fixtures/docs breaks packaged tests | confirmed | fixNow | Replace `/tests/fixtures/main.rs` with `/tests/fixtures/*.rs`; add `/docs/*.md` + `/docs/architecture/*.md`; keep `/tests/*.rs`; relocate the internal review artifact out of the packaged tree | 1 |
| I-1 | Important | `current_revision` ignores superseded/expired; `--touch` overwrites `anchored_at` | confirmed | fixNow | Use real APIs (`memory_write`, `open_memory_store`); touch sets `created_at` + `expires_at`, never `anchored_at`; dead rows feed revision numbering only (supersede chain preserved); TTL rule + surface docs in the same commit | 4 |
| I-2 | Important | NFD Vietnamese split at combining marks | confirmed | fixNow | As review, plus `MEM_FTS_EPOCH` `"2"`→`"3"` so indexed stores rebuild (r2); tests pin one-token NFD, the `İ` behavior, the epoch rebuild, and FTS-level NFC↔NFD cross-match (not Rust token equality, which cannot hold without normalization) | 5 |
| I-3 | Important | judge/freshness run before min_confidence filter | confirmed | fixNow | Insert point inside `analyze_corpus_with` (not the nonexistent `collect_report`); production judge assigns no confidence (its `apply` only rejects, `judge.rs:41-89` — r2 anchor fix) and freshness only demotes (`freshness.rs:314-321`), so pre-filtering is safe; regression uses the real judge-spawn hook with a counting stub | 8 |
| I-4 | Important | allowlist checks `file_changed` not anchor; rejects symbol refs | partially | fixNow | Keep `!refs.is_empty() &&` gate (reject review's inversion); shape-branched freshness (r2): path refs need exact files-row + `file_changed` + anchor-valid, symbol refs use the symbol arm; helper moves to db to avoid a query→service edge | 7 |
| I-5 | Important | p95 Critical downgraded to Warn | confirmed | fixNow | Apply review diff; guard on `status == Pass` first | 2 |
| I-6 | Important | LIKE without ESCAPE; LIMIT without ORDER BY (4 sites) | confirmed | fixNow | Reuse one `like_escape` moved to `src/db.rs` (no copy into mem_query.rs); fix all 4 sites incl. `symbol_loc` + audit `LIMIT 500`; test INSERT corrected to the real `files` schema; tie-break selection pinned | 6 |
| I-7 | Important | saved_tokens prices exit 1/2; `--global` misses worktree fold | confirmed | fixNow | Apply review diff (Ok-only pricing); worktree fold + ephemeral filter in `aggregate_global_usage`; CHANGELOG entry for the public savings shift | 20 |
| I-8 | Important | basename collision; ephemeral ignores worktree root; `canonical_tool` misses audit/mem_reindex | partially | fixNow | Ephemeral fold via `cached_worktree_main_root` (review's field read is a no-op); `canonical_tool` = `is_priced ∪ UNPRICED_TOOLS` (24 names); `discover_repos` re-keyed by path with ancestor-disambiguated display names | 3, 15, 22 |
| I-9 | Important | three bespoke root resolvers diverge from `RepoRef::resolve` | confirmed | fixNow | Rewrite per real API `resolve(Some(start)).ok().map(root().to_path_buf())`; fix all 3 sites (review fixed 1/3); error mapping pinned to explicit-root-only propagation | 14 |
| I-10 | Important | FTS reconcile defeats partial index + recompiles SQL per row; EntryCache key lacks path | confirmed | fixNow | Review diff covers only data.rs; FTS half designed per Technical Plan §8 (UNION ALL + hoisted prepared statements) | 10 |
| I-11 | Important | `is_raw_client` kept in `unresolved_callees`; N+1; missing tie-breakers | confirmed | fixNow | coverage hunk verbatim; finalize context corrected to real fields `file`/`line`; ADD the two missing sub-fixes (N+1 hoist, `judge.rs` ORDER BY `qualname`) | 9 |
| I-12 | Important | `.git` pointer spoof; ANSI injection; unbounded reads/threads | partially | fixNow | Rewrite against real `worktree_main_root(&Path) -> Option<PathBuf>`; ANSI sanitized in `canonical_agent` with its own commit and test (r2 — round 1 promised this without a hunk); bounded line reader + request-line cap + connection cap | 12, 11, 13, 17 |
| I-13 | Important | Memory Store click filter fabricates option, empties list | confirmed | fixNow | Real id `mem-store-filter`; keep toggle behavior by comparing the matched option value; no-match falls back to `ALL`; no JS harness exists so verification is a Rust asset text-guard plus the documented manual check | 18 |
| I-14 | Important | expired notes report `is_current: true` | confirmed | fixNow | Rewrite on the real SQL (`ORDER BY key ASC, rev DESC`); predicate-only change inside `is_current` so the decode closure keeps its column indexes; `expires_at` guarded by the file-local `column_present` (the shared one consolidates later, commit 23) with per-branch parameter binding so v1 stores cannot hit InvalidParameterCount | 19 |
| I-15 | Important | worktree rows collapse per SSE render; agent tables lack keyboard access | partially | fixNow | Rewrite against real identifiers (`agent-row-item`, no `tr.title` today, no `(global)` branch — review fiction removed in r2); cover BOTH agent tables; expansion persistence + label filter target with stopPropagation + aria cleanup | 18 |
| I-16 | Important | ephemeral filter scattered; `--repo` filter differs TUI vs linear | partially | fixNow | Pass the real `global` flag into `handle_memory_touch` (review's `root.is_none()` proxy is wrong when `--root` is set in global mode); unify on one predicate; ADD the monitor ingest half (both loops) that round 1 silently dropped | 15, 16 |
| I-17 | Important | health.md `--key`; MCP reuse default `""`; `--all-tools` count | confirmed | fixNow | Test uses `engram::service::TOOL_SPECS` const (not a `tool_specs(true)` fn); extend to README:155 mem list + `cli.rs` mem help doc-comment per the sync gate | 24 |
| S-1 | Suggestion | 3x duplicated helpers; 9 oversized functions | partially | fixNow (dedup) | Dedup rides commits 6 and 23; splits deferred (Non-Goals) | 23 |
| S-2 | Suggestion | `kill_group` runs even on clean judge exit | partially | defer | Claim true (`judge.rs:249` unconditional call) but the fix contradicts the documented orphan-prevention intent (`judge.rs:247-249`) and leaks helpers/pipe; benefit unmeasured | — |
| S-3 | Suggestion | `has_jwt` misses quoted/assigned JWTs | confirmed | fixNow | Rewrite with `match_indices` + existing `boundary`; keep `Vec<&str>` parts; add missing negative/positive cases | 21 |
| S-4 | Suggestion | macro scan falls back to unbounded on mid-char cap | confirmed | fixNow | Review diff verbatim (boundary-walk proven by reconciliation's rustc probe); add one multi-byte-cap unit test | 22 |
| S-5 | Suggestion | MCP mem_query error prefix `mem_query` vs `mem query` | confirmed | fixNow | Review diff verbatim; add MCP parity test | 24 |
| S-6 | Suggestion | dead `> 50 MiB` quick_check branch | confirmed | defer | Confirmed dead (`audit/memory.rs:101-104` vs `:120-121`); both resolutions change audit policy with no written contract; owner decision (Open Questions) | — |
| N-1 | Nit | `prune_stale_entries_all` called twice at startup | confirmed | fixNow | Remove the first call (`server.rs:82`; the `:93` call and the ephemeral prune remain) | 15 |
| N-2 | Nit | HEAD `/events` ACAO; `host_allowed` breaks `[::1]:port` | confirmed | fixNow | Two-branch extraction; bracket-less IPv6 Host forms (`::1`, `::1:8080`) stay rejected per RFC 7230 so the allowed set is genuinely unchanged; ACAO removed at `server.rs:426` | 17 |
| N-3 | Nit | MCP footer examples stale | confirmed | fixNow | Review diff verbatim (string verified against `format_footer`, `format.rs:72-81`) | 24 |

## Round-2 Resolution Map

| # | Council blocking finding | Resolution |
|---|---|---|
| B1/B6 | CUJ 4 / commit 5 test `mem_tokens_unicode(nfd) == mem_tokens_unicode(nfc)` cannot pass (no normalization in the tokenizer) | CUJ 4 Then rewritten; commit 5 asserts one-token-per-word for both spellings + FTS-level cross-match after epoch rebuild; Rust token equality dropped |
| B2 | Commit 18 unconditional `?1` binding breaks stores without `expires_at` (InvalidParameterCount) | Per-branch binding: `[now]` when the column exists, `[]` when not; v1-store test added |
| B3/B10 | ANSI sanitize promised but no commit touched `src/journal/agent.rs` | New commit 13 with the sanitize hunk + unit test; disposition and ledger aligned |
| B4 | Monitor half of I-16 absent (no `entry_is_ephemeral` in monitor ingest) | Commit 16 filters both ingest loops (`mod.rs:236`, `:305`) gated on global-journal mode; test added |
| B5 | Commit 7 ambiguous between keeping the files-probe (symbol refs still rejected) and dropping it (suffix-match suppression regression) | Shape-branched design (Technical Plan §5): path refs keep the exact-files precondition; symbol refs use the symbol arm; moved-file negative test added |
| B7 | Commit 8 cited a nonexistent "journal/spy harness" in tests/cli/reuse.rs | Test moved to tests/cli/reuse_phase5.rs (which owns `EG_JUDGE_BIN`/`EG_JUDGE_MODEL`); harness is declared NEW: a counting stub script asserting an empty invocation file |
| B8/B11/B14 | Commit 16 mechanism and test contradicted each other on bare `::1` | RFC 7230 decision: bracket-less IPv6 Host forms are rejected; test list updated; allowed set unchanged (today's behavior for bare `::1` is rejection — verified `http.rs:87-92`) |
| B9 | Tokenizer change without the designed migration (old NFD rows stay broken; NFD-note↔NFD-query pairs regress) | Commit 5 bumps `MEM_FTS_EPOCH` `"2"`→`"3"`; self-healing rebuild on write-open, legacy read fallback, revert-safe; "query-shape only" claim corrected |
| B12 | Commit 3 claimed mem_reindex routing "matches classify" — false (`summary.rs:45-49` has no mem_reindex) | Routing justified on semantics; classify divergence documented as pre-existing and untouched |
| B13/B22 | Commit 18 depended on `column_present` promoted only in commit 22 (cycle) | Commit 19 uses the file-local private helper; the promote stays in commit 23; dependency list corrected |
| B15 | Plan violated the hard 500-LOC gate while forbidding new modules | §Technical Plan allows `src/dashboard/data/ingest.rs` + per-file `tests.rs` submodules; per-file LOC budget table added |
| B16 | Commit 7 would create the crate's first query→service edge | `check_ref_status` + `RefStatus` move to `src/db/query_state.rs` (commit 7); both consumers call downward |
| B17 | No CHANGELOG hunks despite AGENTS.md §5.4 and repo practice | `## [Unreleased]` section created in commit 4; entries in commits 4, 5, 20; finalized in commit 24 |
| B18 | Touch TTL semantics "documented" nowhere | Commit 4 adds memory-store.md Write/Touch bullets, cli-reference `--touch` row, `SKILL.md` pillar-3 line, agent-integration `mem_write` row |
| B19 | Audit error mapping impossible (all discovery errors are `Usage`) | Rule pinned: propagate only when `root_arg.is_some()`; no-root failures keep today's `NoResults` exit 1 + hint |
| B20 | `docs/review.md` would fail hygiene if committed and ship in the crate via `/docs/*.md`; the design doc's link would break if it moved | Commit 1 moves it to `docs/baselines/2026-11-14-eg-v1-0-6-review.md` (hygiene-excluded, outside the include globs); all spec links updated; negative packaging guard added |
| B21 | Four verification commands named nonexistent test targets | All rewritten against the real targets (`cli`, `mcp`, `fixtures`, `reuse_bench` per `cargo metadata`); commit 14 gained its missing `Command:` lines |

Advisories adopted in r2 (abridged): `test_support` fixtures everywhere (also required by `write_path_check.sh`), `İ` pin + boundary tests (`expires_at == now`, `oldest_ts == b_start`, exactly-256-KiB line), supersede-chain preservation (option b), judge evidence anchor (`judge.rs:41-89`), rotation-vs-read-cap numbers (5 MiB / 16 MiB), connection-cap 503 stderr line, request-line cap, `read_snippet` included in the capped-reader migration, asset text-guard test, stopPropagation on the label target, toggle compares matched value, aria-label removed on all filter rows, `(global)` fiction deleted, `display_name_for` helper, label-uniqueness note, README anchor correction, `format_footer` at `format.rs:72`, monitor explicit-root assertion, mcp.rs:91 follow-up note, cargo-package verification count fixed (6 top-level fixture `.rs` files, 12 including `memory/**` and `reuse/**`), no-refs pin as e2e, silent-skip contract documented. Rejected r2 advisories: adding a `skipped_lines` payload field (doc-only contract chosen — observability follow-up), EntryCache behavioral test (structural change, existing suite covers the consumer), JS harness and CI cargo-package (Non-Goals), back-pointer verification for worktree pointers (residual risk documented instead — same-user threat model).

## Round-3 Resolution Map

| # | Council round-2 blocking finding | Resolution |
|---|---|---|
| B1/B4 | LOC budget missing `src/dashboard/data/reuse.rs` (measured 498, two lines under the cap) while commit 6 patches `symbol_loc` there; the "others touched < 360" row was false | Budget table gains the reuse.rs row with the decision pinned: commit 6 moves both inline test modules (`reuse.rs:437-438` and `478`) to `src/dashboard/data/reuse/tests.rs`; the others row now names the over-360 files (`src/query/reuse.rs` 422→~432, `src/cli.rs` 433, `src/service/args.rs` 374) |
| B2 | Commit 6's `include_str!("../../schema/index.sql")` resolves to the nonexistent `src/schema/` from the new `src/service/mem_query/tests.rs` home | Path corrected to `../../../schema/index.sql` (three levels up to repo-root `schema/`); the only other `include_str!` use in the plan is commit 18's asset guard, whose `../../../src/...` path from `tests/cli/dashboard/assets.rs` is correct |
| B3 | Commit 1 would track the review artifact with 95 workstation-path matches — a tracked-docs leakage hole the hygiene exclusion merely hides | Commit 1 now redacts absolute workstation paths to `<repo-root>` before tracking, and the packaging guard asserts the relocated baseline matches no leak pattern — enforcement, not exclusion |
| B5 | Commit 18 claimed "`announce()` already announces filter actions" — false: no filter handler announces anything (verified: every `announce(` site is a toggle/reset/selection/verify/stream event) | Sentence corrected; the removal now comes with one `announce('Filtered by <label>')` per filter handler (4 handlers), pinned by the asset text-guard |
| B6 | The spec file itself contained a literal workstation-path pattern at its Commit 1 line and failed `hygiene_check.sh` on itself (`docs/designs/*` is not excluded) | Line reworded to "95 workstation-path matches"; the check is now part of the Verification Plan and passes (see below) |

Round-2 advisories adopted in r3 (abridged): hygiene-gate anchor corrected to `hygiene_check.sh:79-95` (`MAX_LOC=500` at `:80`, hard fail `:94-95` — the `:59-70` citation pointed at the zero-emoji section); the `mem_ops` citations, which were missing the `service/` directory prefix (the real file is `src/service/mem_ops.rs:137-141`), replaced by the verified read-degrade chain (`memory_fts.rs:57-58` → `memory/guard.rs:106-108` → `memory_query.rs:125-129`) in all four sites; `fetch_symbol_body` renamed to the real `source()` (`judge.rs:162-197`); commit 14's mcp tests described as a new out-of-line file (mcp.rs has no test module); commit 13's filter extended to bidi/format characters and its write-time-only residual documented; the `path:line` splitting dropped so suppression and display agree; commit 19's v1 fixture pinned to the filtered-schema mechanism; commit 5's FTS cross-match test names the exact public pair (`mem_query_terms` + `mem_tokens`, `tokens.rs:196-201`); commit 8's judge-hook anchors corrected (`reuse_phase5.rs:41-42`, `:95-96`); commit 11's capped reader marked `pub(super)` and `read_snippet`/`Tail` anchors corrected (189-219, 221-318); rebuild-runs-to-completion sentence added to Risks; `analyze_corpus_with` added to the S-1 churn list; commit 15/18/24 minor anchors corrected (liveness tests at 165, README list at 155, cli-reference row at 237); query_state budget unified at +~45 → ~425 in both places.

## Round-4 Resolution Map

| # | Council round-3 blocking finding | Resolution |
|---|---|---|
| B1 | The Commit 1 guard unconditionally walked `docs/baselines/*.md`, which is absent inside a packaged crate — the exact scenario CUJ 2 requires green — and the file's house style (unwrap) would turn the missing directory into a panic | The walk is now conditional (`if let Ok(entries) = fs::read_dir(...)`) with the rationale pinned in Commit 1: the leak assertion runs in a source checkout where the baseline is tracked and stays a no-op in the packaged crate, whose include list refuses `/docs/baselines/**` by construction |
| B2 | Technical Plan §10 still carried the false "announce() already gives filter feedback" claim and understated the scope as "three tables", contradicting the corrected Commit 18 item 4 | §10 synced to item 4: four tables / five sites (543/546, 657, 720, 835), and "the four filter handlers gain one `announce('Filtered by <label>')` call each (Commit 18 item 4, pinned by the asset text-guard)" |

Round-3 advisories adopted in r4 (abridged): the README anchor reverted to the verified lines (mem list at `README.md:155`, heading at `:153` — r2 was right, the r3 "correction" was off by one on both numbers, re-verified this session by `grep -n "eg mem write|query" README.md` → 155 and `grep -n "### Platform & Maintenance Commands" README.md` → 153); the Round-3 map's over-360 list now spells `src/query/reuse.rs` to disambiguate from `src/dashboard/data/reuse.rs`; the round-3 label "fictional `src/db/mem_ops.rs`" corrected — the file and lines are real at `src/service/mem_ops.rs:137-141`, the r2 citation merely lacked the directory prefix; commit 6 now moves both inline test modules (attributes at `reuse.rs:437-438` and `478`, grep-verified) so the ~442 budget holds; commit 13's test pins the bidi ranges with `U+202E` and `U+200B` inputs.

## Round-5 Resolution Map

| # | Council round-4 blocking finding | Resolution |
|---|---|---|
| B1 | The LOC budget table still lacked the tests/ side: `tests/cli/reuse.rs` (477) + commit 7's three e2e tests ≈ ~520-530, `tests/cli/p8_impact.rs` (476) + commit 1's guard ≈ ~491-501, `tests/cli/dashboard.rs` (451) + `mod assets;` ≈ ~453 — the same near-cap class as r2-B15 and r3-B1/B4, hard-failing the hygiene gate | Commit 7's e2e tests move to a new `tests/cli/reuse_allowlist.rs` (~130 lines, registered `mod reuse_allowlist;`), commit 1's packaging guard and commit 24's surface tests move to a new `tests/cli/packaging.rs` (~70 lines, registered `mod packaging;`) — `tests/cli/reuse.rs` and `tests/cli/p8_impact.rs` are untouched by the wave (`tests/cli/main.rs` 31 → ~33); the budget table gains five tests-side rows and the "others" row now reads "< 440 each (src side)" |

Round-4 advisories adopted in r5 (abridged): the dashboard reuse.rs budget row now names both inline test modules (437-438 and 478) and unifies the result at ~440 with commit 6's text; the read-degrade citations gain the direct gate `src/service/mem_ops.rs:137-141` alongside the verified three-link chain (real file, verified at `:137-138` this session); the r4 map labels were already corrected in r4 (README 155/153 re-verified by `grep -n "eg mem write|query" README.md` → 155 and `grep -n "### Platform & Maintenance Commands" README.md` → 153; `src/query/reuse.rs` spelled out in the r3 map; `mem_ops` labeled a missing-prefix note, not a fictional file).

## Round-6 Resolution Map

| # | Arbitration finding | Resolution |
|---|---|---|
| B1 | Commit 9's planned coverage test `coverage_raw_client_not_in_unresolved` ("a `fetch` callee no longer reaches `unresolved_candidates`") contradicts the shipped diff, which removes the `|| is_raw_client` disjunct only from the `unresolved_callees` retain (`coverage.rs:44-45`; `all_callees` keeps it at `:42-43`) — and that disjunct is unreachable dead code, verified two ways: the static `STOPLIST` (`src/query/reuse/tokens.rs:10-72`) contains no `RAW_CLIENTS` member (`Connection::open`, `Connection.open`, `sqlite3_open`, `requests.get`, `requests.post`, `axios`, `axios.get`, `axios.post`, `fetch`, `reqwest::get`, `reqwest.get`, `ureq::get`, `ureq.get` — each checked against `tokens.rs`; only the bare `"open"`/`"get"` tokens exist, and `stopset.contains` is exact-string membership, so a compound raw-client callee never matches), and `build_stoplist`'s frequency guard never stoplists a raw client (`tokens.rs:166`, `&& !is_raw_client(&callee)`). No input can make the removed disjunct observable, so the planned assertion could never fail on either side of the hunk; the shipped test `coverage_raw_client_still_matches_user_defined_symbol` (`coverage.rs:332-357`) pins the real contract in both directions. | Keep the hunk — it is dead-code removal, behavior-neutral, and no test can distinguish it. No code change. The Commit 9 test entry above is renamed to the shipped name with the two-way semantics (user-defined `fetch` stays a D1L match; stoplisted `unwrap` does not reach the pool). The alternative (reverting the hunk to keep the spec letter) was not taken: it would reintroduce dead code. |
| B2 | The 24-commit ledger omits hunks the wave ships (grep + section read): Commit 4 also modifies `src/db/memory_query.rs` — `memory_get`'s current-row pick gains `ORDER BY rev DESC LIMIT 1` plus the two-rows-both-active doc comment (`memory_query.rs:262-272`); without it, an expired row re-written to rev 2 leaves two `superseded_by IS NULL` rows and the pick follows rowid order. Commit 5 also modifies `schema/memory.sql` (the Unicode-index reads-gate comment moves to `fts_epoch = 3`, `schema/memory.sql:57`) and `src/db/memory_v2_tests.rs` (the epoch assertions flip: rebuilt store reads `"3"`, fresh build reads `"3"`), while Commit 5's `memory_fts.rs` scope note says "Nothing else". | The hunks are correct and required (CUJ 3's read assertions depend on the deterministic pick; the epoch tests fail without the flipped assertions). The Commit 4 and Commit 5 file scopes are extended below so the ledger matches the working tree and each hunk stays inside the commit carrying its behavior; the "Nothing else" note still holds for the `memory_fts.rs` file itself. |
| B3 | Seven further hunks ship with the wave's behavior but were absent from their commits' ledgers, so a selective revert of Commit 4 per the old ledger would produce a mixed state (write-side gating reverted while the dashboard gate stayed) and review sign-off against the ledger would cover a different change than what ships: (1) `memory_touch_if_rev`'s UPDATE gains the expiry term + rewritten doc (`memory_write.rs:431-460`); (2) `memory_touch`'s doc comment rewritten from "consistent with `AND superseded_by IS NULL`" to the same two-term predicate (`memory_write.rs:461-467`); (3) `src/cli.rs`'s clap doc-comment on `--touch` rewritten to name the TTL re-assertion; (4) `src/service/tools/memory.rs`'s `mem_write` ParamSpec `description` rewritten the same way (both machine faces are AGENTS.md §5.1/§5.2 obligations of the flag change); (5) the dashboard picker probe in `src/dashboard/server/touch.rs` (~90-104) gains the expiry term, `ORDER BY rev DESC`, and the two-row comment; (6) the e2e `dashboard_touch_rejects_expired_key` in `tests/cli/dashboard/touch.rs` (~line 49); (7, r6 round three) `supersede_cross_key`'s target probe gains `ORDER BY rev DESC LIMIT 1` and its doc comment states the pick in SQL (`memory_write.rs:397-418`) — after an expired-key re-write the target can carry two `superseded_by IS NULL` rows, and a selective revert per the old ledger would restore a pick that borrows its correctness from the `memories_current` partial index (verified: with that index dropped, the unordered probe picks the dead expired rev 1 in both insertion orders) while the sibling readers (`current_revision`, `memory_get`, the dashboard picker) state it in SQL, so a planner leaving that index could mark the wrong target revision. | All seven hunks are correct and required; no code change. The Commit 4 scope (items 1-4 and 7), the Commit 4 surface-docs list, and the Commit 15 scope (items 5-6) are extended in §Detailed Implementation Plan so the ledger matches the working tree. Items 3-4 stay in Commit 4's surface-docs set (the flag's own sync duty); mapping them to Commit 24 instead is the documented alternative if the owner prefers. Item 7 carries no `expires_at` term on the probe by owner scope: whether superseding a dead target should be a contract decision stays with the owner, and today's behavior (pick the highest non-superseded revision even if expired) is deliberate and consistent with dead-key re-writes being allowed. The user-facing "both touch paths" wording is corrected to three paths in the same correction (CHANGELOG `[Unreleased]` and `docs/architecture/memory-store.md`): the dashboard's `/api/memory/touch` also changed behavior (404/409 instead of re-anchoring a dead revision). |

## Detailed Implementation Plan

### Commit 1 — `fix(packaging): include test fixtures and docs in the crate package` (C-2)

#### Cargo.toml (Modify)

- Scope: package include list (lines 15-43).
- Change: replace the entry `/tests/fixtures/main.rs` with `/tests/fixtures/*.rs`; add `/docs/*.md` and `/docs/architecture/*.md` after the tests entries. All other entries unchanged; nothing under `/docs/baselines/**` is added.
- Rationale: `tests/fixtures/main.rs` declares `mod common; mod misc; mod python; mod rust; mod typescript;` — a single-file include breaks module resolution in the packaged crate; `tests/cli/p8_impact.rs` reads `docs/` at depth 1. Top-level tracked docs are hygiene-clean today (the gate passes with only `docs/review.md` untracked).

#### docs/baselines/2026-11-14-eg-v1-0-6-review.md (Create, moved and redacted)

- Scope: relocation of the untracked `docs/review.md` (95 workstation-path matches measured this session).
- Change: move the file to `docs/baselines/2026-11-14-eg-v1-0-6-review.md`, then redact before it is ever tracked: every absolute workstation path (the repository prefix and any `~`-rooted home paths) becomes the placeholder `<repo-root>` (and `~/…` stays literal where already tilde-rooted), so the tracked baseline carries no private leakage per `AGENTS.md §6` — the `docs/baselines/*` hygiene exclusion (`tests/scripts/hygiene_check.sh:37`) keeps the gate from scanning the directory, and redaction closes the gap that exclusion alone would leave; the `/docs/*.md` top-level glob cannot reach the file either.
- Rationale: committing it unredacted would track workstation paths; leaving it untracked breaks the design doc's relative link. Redaction keeps the audit content readable (`<repo-root>/src/audit/collect.rs` style references) while satisfying the leakage invariant, and the guard below enforces it instead of trusting the exclusion. The slug matches the artifact-slug rule (lowercase digits-and-dashes only).

#### tests/cli/packaging.rs (Create) — registered as `mod packaging;` in tests/cli/main.rs (r5)

- Scope: packaging guards, in their own new file (`tests/cli/p8_impact.rs` measures 476 — r5 council finding; appending here would push it to ~491-501, so the guard gets a dedicated home and `p8_impact.rs` stays untouched by commit 1).
- Change: one test that reads `Cargo.toml` from `env!("CARGO_MANIFEST_DIR")` and asserts the text contains the `/tests/fixtures/*.rs` (or `/tests/fixtures/**`) and `/docs/*.md` include entries and does not contain `/docs/baselines`; the same test then walks `docs/baselines/*.md` only when the directory exists (`if let Ok(entries) = fs::read_dir(root.join("docs/baselines"))`) and asserts none of the leak patterns hygiene enforces (workstation absolute paths) appear in the relocated, redacted baseline — enforcement instead of exclusion alone (r3). The absence guard is deliberate (r4): inside a packaged crate the `docs/baselines/` tree is absent by construction (the include list refuses `/docs/baselines/**`), and the house style in the adjacent files is unwrap — so an unconditional walk would panic the packaged run that CUJ 2 requires green; in a source checkout, where the baseline is tracked, the directory exists and the leak assertion runs.
- Rationale: text-containment guard is the minimal regression fence; a full `cargo package` in CI is out of scope for this wave.
- Tests: `cargo test --locked --test cli packaging` green; then `cargo package --list | grep -E 'tests/fixtures/.*\.rs$' | wc -l` prints 6 (the top-level fixture modules; 12 if counting the `memory/**` and `reuse/**` fixture files, which are covered by their existing globs).

### Commit 2 — `fix(audit): restore baseline sufficiency and keep Critical severity` (C-1, I-5)

#### src/audit/collect.rs (Modify)

- Scope: baseline sufficiency computation (lines 287-292) + test-module declaration.
- Change: compute `clipped` from `coverage.oldest_ts` (the min over all valid entries of both generations, `collect.rs:262`): true when `oldest_ts > b_start`, false when `oldest_ts` is at or before `b_start` or absent (absent is already caught by `baseline_metrics.entries > 0`). Remove the `baseline_metrics.oldest_ts` intermediate when it becomes unused. Append `#[cfg(test)] mod tests;`.
- API shape: no signature change; `clipped` remains a local; `BaselineInfo` fields unchanged.
- Data flow: unchanged — only the clipping predicate input moves from the baseline bucket's oldest to the coverage-wide oldest.
- Rationale: `baseline_metrics` only admits entries with `ts >= b_start` (`collect.rs:270-272`), so its own oldest can never prove window coverage; `coverage.oldest_ts` is the only signal that rotation dropped the pre-`b_start` history. Verified: the corrected formula passes the entire existing suite (reconciliation run).
- LOC budget: 464 − ~28 (KNOWN array leaves in commit 3) + ~6 → ~442.

#### src/audit/collect/tests.rs (Create)

- Scope: commit 2 and commit 3 regression tests (file-module test submodule, `render/tests.rs` pattern).
- Change: two tests sharing one seed helper built exclusively through `crate::db::test_support::{temp_root, write_file, make_dir_all}` (inline `fs::write` outside `db.rs` would fail `write_path_check.sh`):
  1. `baseline_sufficient_when_journal_precedes_baseline_start` — one valid entry at `b_start - 86_400`, one exactly at `b_start` (pins the strict-`>` boundary), and 24 successful `callers` entries spread over 4 distinct days inside the baseline window; asserts `clipped == false` and `sufficient == true`.
  2. `baseline_clipped_when_journal_starts_inside_baseline_window` — the same 24 entries but no entry at or before `b_start`; asserts `clipped == true` and `sufficient == false`.
  Both call `collect(&Mode::Local(root), now, 30, 30)`.
- Command: `cargo test --locked --lib audit::collect`.

#### src/audit/checks/runtime.rs (Modify) and src/audit/checks/runtime/tests.rs (Create)

- Scope: `performance` check, baseline-regression branch (~line 167) + its tests.
- Change: gate the baseline-regression escalation on the current status being `Pass` before overwriting with `Warn`; `Critical` and `Warn` from the static thresholds survive. Append `#[cfg(test)] mod tests;`.
- API shape: no change (`performance` stays `pub(super) fn performance(&AuditData) -> CheckResult`).
- Tests (tests.rs): `performance_critical_not_downgraded_by_baseline_regression` — seed a journal whose baseline window holds 24 fast successful `callers` entries (sufficient baseline) and whose recent window holds 24 entries with `latency_us = 20_000_000` (far above any per-tool critical); call `super::performance(&collect(...))` and assert `Status::Critical`; a second case with slow-but-sub-critical latency asserts `Warn` via the baseline rule.
- Command: `cargo test --locked --lib audit::checks::runtime`.

### Commit 3 — `fix(audit): classify audit and mem_reindex in telemetry facts` (I-8, part canonical_tool/Metrics)

#### src/audit/collect.rs (Modify)

- Scope: `canonical_tool` (lines 411-441) and `Metrics::add` tool routing (lines 143-161).
- Change: `canonical_tool` membership becomes `is_priced(tool) || crate::journal::UNPRICED_TOOLS.contains(&tool)` and the 22-name `KNOWN` array is deleted. In `Metrics::add`, route `mem_export` into `mem_reads` (restores parity with `summary.rs:45-49`, verified) and `mem_reindex` into `mem_writes` on its own semantics (a reindex mutates the store's derived state). `classify`'s `mem_write` arm not listing `mem_reindex` is a pre-existing dashboard-side divergence, documented here and untouched — this commit must not touch `summary.rs`.
- Tests (collect/tests.rs): `canonical_tool_recognizes_audit_and_mem_reindex` (both map to themselves, unknown maps to `other`) plus one `Metrics::add` classification assertion per rerouted tool.
- Command: `cargo test --locked --lib audit::collect`.

### Commit 4 — `fix(memory): gate writes and touches on active revisions` (I-1)

#### src/db/memory_write.rs (Modify)

- Scope: `memory_write_batch` dead-row branch (lines 179-187), `Revision` struct (219-223), `try_write_revision` hash-match branch (239-257) and supersede call (267-269), `current_revision` (317-331), `memory_touch` (416-431), `memory_touch_if_rev` (SQL + doc, r6 ledger correction per Round-6 Resolution Map B3), `supersede_cross_key` (probe ORDER BY + doc, r6 ledger correction per B3 item 7); test-module declaration.
- Change (seven coupled edits, one behavior contract):
  1. `Revision` gains `superseded_by: Option<i64>` and `expires_at: i64` plus an `is_active(now)` method: no superseder and (`expires_at == 0` or `expires_at > now`). `current_revision`'s SELECT adds both columns.
  2. `memory_write_batch` and `try_write_revision` treat an inactive current row as live-only-for-numbering: identical-content rewrites fall through to `insert_revision` instead of returning `Duplicate`/`Touched`; non-force conflicts only block against active rows.
  3. When the current row is inactive, `supersede_current` is skipped — the dead row keeps its original `superseded_by`/`superseded_at` (Technical Plan §3). When active, behavior is unchanged.
  4. The `note.touch` branch updates `created_at = now` and `expires_at = note.expires_at` only — `anchored_at` is removed from that UPDATE (contract: `mem touch` owns `anchored_at`, `memory-store.md:119-120`).
  5. `memory_touch` returns `Ok(None)` when the current revision is inactive (superseded or expired); `memory_touch_if_rev`'s UPDATE carries the same two-term SQL predicate `superseded_by IS NULL AND (expires_at = 0 OR expires_at > ?4)` bound to `now`, so both touch paths and the dashboard's revision-preconditioned touch refuse dead revisions (r6 ledger correction — the original sentence described the one-term pre-wave shape at `memory_write.rs:402`, which this commit replaces; see Round-6 Resolution Map B3).
  6. (r6 ledger correction, B3) `memory_touch_if_rev` (SQL + doc comment) and the rewritten `memory_touch` doc comment (the "consistent with `AND superseded_by IS NULL`" sentence becomes the same two-term predicate) ship in this commit — they implement the touch-side half of item 5's contract.
  7. (r6 ledger correction, B3 item 7) `supersede_cross_key`'s target probe gains `ORDER BY rev DESC LIMIT 1` and its doc comment states the pick in SQL like `current_revision` and `memory_get` instead of borrowing it from the `memories_current` partial index's scan order — after an expired-key re-write the target can carry two `superseded_by IS NULL` rows and the highest revision must win under any planner. No `expires_at` term on the probe: whether superseding a dead target should fail closed is an owner contract decision (B3 item 7), and today's behavior is deliberate.
- API shape: `memory_touch(&mut Connection, &str, i64) -> Result<Option<i64>>` unchanged; `Revision`/`is_active` private; `MemWriteOutcome::Written { rev, superseded_rev }` unchanged.
- Data flow: dead-row rewrites append a revision and re-INSERT both FTS rows through the existing insert path; no new tables or columns.
- LOC budget: 431 + ~28 → ~459 (tests externalized).

#### src/db/memory_write/tests.rs (Create)

- Change: five tests on the real APIs (`open_memory_store` at `memory_store.rs:191`, `memory_write` at `memory_write.rs:123`, `memory_get` at `memory_query.rs:263`, `memory_touch_if_rev` at `memory_write.rs:438`), fixtures via `crate::db::test_support`:
  1. `cross_key_superseded_note_rejects_touch_and_can_be_rewritten_with_same_body` — write `k1` at t=100, write `k2` with `supersedes: Some("k1")` at t=200; assert `memory_touch(&mut conn, "k1", 300)` returns `None`; re-write `k1` same body at t=400 asserts `Written { rev: 2, .. }` (the `..` is required: the variant carries `superseded_rev`); `memory_get` returns `Some`; and a SQL read of k1's old row asserts `superseded_by` still points at k2's revision id (chain preserved, Technical Plan §3).
  2. `touch_updates_created_and_expires_not_anchored` — session note with explicit TTL; the `--touch`-equivalent input updates `created_at` and `expires_at` and leaves `anchored_at` unchanged.
  3. `expired_note_is_inactive_at_and_after_deadline` — a session note whose `expires_at = T`; `memory_touch` at `now = T` returns `None` (the `>` boundary makes exactly-at-deadline inactive) and at `T - 1` succeeds; re-write after expiry creates rev 2.
  4. (r6 ledger correction, B3 item 7) `cross_key_supersede_marks_highest_non_superseded_target_rev` (`tests.rs:91`) — k1 carries an expired rev 1 (written at t=100, `expires_at = 500`) and a live rev 2 (re-write at t=600), both with `superseded_by IS NULL`; the superseding write of `k2` at t=700 must mark the highest non-superseded revision (rev 2's `superseded_by` = k2's row id, rev 1's chain untouched) and leave `memory_get(k1)` = `None` (the pick falls to the expired rev 1 and the expiry gate rejects it).
  5. (r6 ledger correction, B3 item 1) `touch_if_rev_refuses_expired_revision` (`tests.rs:153`) — pins `memory_touch_if_rev`'s two-term predicate boundary directly: `Some(1)` at `now = T - 1`, `None` at `now = T` and after (the `>` boundary; no test had exercised this path before r6).
- Command: `cargo test --locked --lib db::memory_write`.

#### src/db/memory_query.rs (Modify) (r6 ledger correction, Round-6 Resolution Map B2)

- Scope: `memory_get`'s current-row pick (~262-272).
- Change: the pick gains `ORDER BY rev DESC LIMIT 1` plus a doc comment recording why: since inactive-revision gating, an expired row can be re-written without being marked superseded, so two rows of one key may both carry `superseded_by IS NULL` and the pick must be deterministic — mirroring `current_revision`'s `ORDER BY rev DESC` (`memory_write.rs`).
- Rationale: the read side (`mem get`, the dashboard panel) must agree with the write side on which row is current; without the ordering the pick follows rowid order and can flip between fresh and incremental stores. This hunk ships in commit 4 but was missing from its file list; CUJ 3's read assertions depend on it.

#### Surface docs for the touch TTL rule (same commit, AGENTS.md §5) (Modify)

- `docs/architecture/memory-store.md`: the Write bullet gains "and re-asserts the invocation's TTL (`expires_at`)" for `--touch`; the Touch bullet stays `anchored_at`-only; one sentence states dead keys (superseded or expired) are unreachable by both.
- `docs/cli-reference.md` (the `mem write --touch` row, line 237): describe the refreshed `created_at` + `expires_at` semantics.
- `skills/engram-first/SKILL.md` (pillar-3 memory section): one clause noting re-assertion re-applies TTL policy.
- `docs/agent-integration.md` (the `mem_write` row, ~line 117): same one-clause note.
- (r6 ledger correction, Round-6 Resolution Map B3) the two machine faces of the flag, per AGENTS.md §5.1/§5.2: `src/cli.rs`'s clap doc-comment on `--touch` and `src/service/tools/memory.rs`'s `mem_write` ParamSpec `description` — both rewritten to name the TTL re-assertion (`expires_at`; explicit `--ttl` wins, else the 7-day session default) so MCP `tools/list` readers learn the same contract the docs carry.

#### CHANGELOG.md (Modify)

- Change: insert a `## [Unreleased]` section after the "Reading this history" block (before `## [1.0.6]` — Keep-a-Changelog format verified at `CHANGELOG.md:8-17`) with the touch-semantics entry; commits 5, 20, 24 append to the same section.
- Rationale: `AGENTS.md §5.4` requires the changelog in the same commit as a behavior change; repo history shows 7/8 recent commits touching it (round-1 council evidence).

### Commit 5 — `fix(memory): keep combining marks inside Unicode tokens` (I-2)

#### src/memory/tokens.rs (Modify)

- Scope: `push_clean` (104-113), `mem_tokens_unicode` run condition (141), new helper, tests mod (exists at 229).
- Change: add a private `is_unicode_word_char(c)` predicate: `c.is_alphanumeric()` or the combining-mark ranges `0x0300..=0x036F`, `0x1AB0..=0x1AFF`, `0x1DC0..=0x1DFF`, `0xFE20..=0xFE2F`. `push_clean` filters on `is_unicode_word_char(c) && c != '\u{0307}'` (preserves the documented `İ` → `i` case-expansion cleanup, doc-comment at tokens.rs:75-77); the `mem_tokens_unicode` run loop admits `is_unicode_word_char(c)`. `mem_tokens_legacy` untouched (frozen contract).
- API shape: `mem_tokens_unicode(&str) -> Vec<String>` unchanged.
- LOC budget: tokens.rs measured 300 this session; additions ~20 + ~30 tests → ~350, inline.
- Rationale: verified by the reconciliation's sqlite3 probe — FTS5 `unicode61 remove_diacritics 2` folds but never splits `Mn` marks, so keeping them inside the token restores NFC↔NFD cross-matching at query time; the four ranges cover Vietnamese tone marks (`0x0300-0x0309`, `0x031B`) and the general combining blocks.

#### src/db/memory_fts.rs (Modify)

- Scope: tokenizer contract constant (line 8).
- Change: `MEM_FTS_EPOCH` `"2"` → `"3"`. Nothing else: the mismatch branch at `memory_fts.rs:88-99` already drops and rebuilds `memories_fts_v2`, resets `fts_version = 0`, and the chunked reconcile reindexes every store on its next write open (`memory_store.rs:284-285`); reads degrade to the legacy table while a rebuild is pending (`src/db/memory_fts.rs:57-58` → `src/memory/guard.rs:106-108` → `src/db/memory_query.rs:125-129` (direct legacy/v2 gate: `src/service/mem_ops.rs:137-141`)); a reverted binary sees epoch `"3"` and rebuilds back under the old tokenizer.
- Rationale: the token change alters index content, not query shape — without the epoch bump, NFD notes indexed before the fix keep broken tokens forever and NFD-note↔NFD-query pairs would regress (round-1 council finding B9). `eg mem reindex` stays the manual escape hatch (`src/service/mem_reindex.rs:12`).

#### tests (tokens.rs tests mod at 230 + a new inline `#[cfg(test)] mod tests` in src/db/memory_fts.rs, created by this commit and extended by commit 10)

- `mem_tokens_unicode_keeps_nfd_syllable_intact` (tokens.rs mod) — the NFD spelling of `tiếng` yields exactly one token, and the NFC spelling also yields exactly one token (the equality assertion round 1 planned cannot hold — the tokenizer does not normalize; removed).
- `mem_tokens_unicode_drops_turkic_dot_after_lowercase` (tokens.rs mod) — `İ` produces the bare `i` token (pins the `\u{0307}` exclusion).
- `fts_epoch_mismatch_rebuilds_unicode_index` (memory_fts mod) — open a store, set `memory_meta.fts_epoch` to `"2"` via SQL, open for write, assert the epoch row reads `"3"` and rows carry `fts_version = 2`.
- `nfd_body_matches_nfc_query_at_fts_layer` (memory_fts mod) — through the real store API (`open_memory_store` + `memory_write` via `crate::db::test_support` fixtures): write a note whose body is NFD, build the MATCH query from the NFC spelling via the public pair `crate::memory::mem_query_terms(nfc_text)` (wrapping `mem_tokens_unicode`, `tokens.rs:196-201`) and run `SELECT rowid FROM memories_fts_v2 WHERE memories_fts_v2 MATCH ?`; assert the NFD note's rowid is returned (and the mirrored NFD-query↔NFC-note direction) — the cross-match contract lives at the FTS5 fold, not in Rust token equality.
- Command: `cargo test --locked --lib memory::tokens` and `cargo test --locked --lib db::memory_fts`.

#### schema/memory.sql and src/db/memory_v2_tests.rs (Modify) (r6 ledger correction, Round-6 Resolution Map B2)

- `schema/memory.sql`: the Unicode-index reads-gate comment updates to `fts_epoch = 3` (comment at `:57`) so the schema file agrees with the bumped `MEM_FTS_EPOCH`.
- `src/db/memory_v2_tests.rs`: the epoch assertions flip with the constant — `epoch_change_forces_rebuild` asserts the rebuilt store's `fts_epoch` reads `"3"` (`:111`) and `fresh_and_migrated_schema_match` pins the fresh build's `"3"` (`:252`).
- Rationale: without these hunks the bumped constant would contradict the schema comment and fail the dual-FTS invariant tests; they belong to commit 5's single behavior contract (the epoch migration). The "Nothing else" scope note still holds for the `memory_fts.rs` file itself. These hunks ship in commit 5 but were missing from its file list.

#### CHANGELOG.md (Modify)

- Entry under `## [Unreleased]`: Vietnamese NFD notes become findable; existing stores reindex their Unicode FTS automatically on the next write (epoch 3).

### Commit 6 — `fix(db): escape LIKE patterns and order before LIMIT` (I-6 + S-1 escape_like dedup)

#### src/db.rs (Modify)

- Scope: shared SQL text helpers.
- Change: add `pub(crate) fn like_escape(raw: &str) -> String` (escapes `%`, `_`, `\`) — moved from the private copy at `src/dashboard/data/memories.rs:154-163` (identical body, doc-comment preserved).
- LOC budget: 459 + ~14 → ~473.
- Rationale: `src/db.rs` is the lowest layer already imported by most consumers; adding a pure string helper introduces no cycle and satisfies the AGENTS.md reuse ladder over a fourth copy. Honest edge note (r2): `coverage.rs` and `target.rs` gain two new downward query→db edges when their copies fold in (commit 23) — downward, no cycle.

#### src/service/mem_query.rs (Modify) and src/service/mem_query/tests.rs (Create) — the helper itself moves to db in commit 7; this commit fixes the SQL shape and adds the regression tests

- Scope: `check_ref_status` (88-121); test-module declaration + tests file (file measures 422 LOC — externalized per the LOC budget table).
- Change: the file-lookup query gains `ESCAPE '\'` on the LIKE term, a deterministic `ORDER BY` of exact-match-first then newest mtime then path, and the pattern argument becomes the escaped form; call `crate::db::like_escape`. Append `#[cfg(test)] mod tests;`.
- Tests (tests.rs — path anchors are relative to `src/service/mem_query/tests.rs`): `check_ref_status_escapes_underscore_in_like_pattern` — in-memory connection, `execute_batch(include_str!("../../../schema/index.sql"))` (three levels up from the sibling tests file to the repo-root `schema/`), one INSERT into `files` with the real columns `(id, path, size, mtime, content_hash, parse_status, error_nodes)` (the review's INSERT names a nonexistent `lang` column and omits two NOT NULLs — corrected), then asserts a `foo_bar.rs` ref does not match `src/fooXbar.rs` (`RefStatus::Broken`); `check_ref_status_prefers_exact_over_suffix` — two rows whose paths both satisfy the predicate with different mtimes; assert the exact-path row wins deterministically (pins the shared ORDER BY contract used by all four sites).
- Command: `cargo test --locked --lib service::mem_query`.

#### src/dashboard/data/memories.rs, src/dashboard/data/reuse.rs (+ src/dashboard/data/reuse/tests.rs Create), src/audit/memory.rs (Modify)

- `resolve_single_ref` (165-219): keep the existing `ESCAPE '\'` + escaping and add deterministic `ORDER BY` (exact-path-first / mtime / path for the file arm; `line_start` / `path` / `qualname` for the symbol arm) before both `LIMIT 1`s; re-point at `crate::db::like_escape`; the private `like_escape` is deleted.
- `symbol_loc` (`reuse.rs:45-65`): add `ESCAPE '\'`, escape the suffix pattern via `crate::db::like_escape`, add deterministic `ORDER BY` before `LIMIT 1`.
- `src/dashboard/data/reuse.rs` LOC plan (r3/r4): the file measures 498 — two lines under the hygiene cap — and carries two inline test modules (attributes at `reuse.rs:437-438` and `478`, grep-verified); the commit moves both out to `src/dashboard/data/reuse/tests.rs` unchanged, keeping the file at ~440 after the +4 SQL-shape lines and preserving the gate (moving only the first module would leave ~461 — still under the cap, but the ~442 budget assumes both).
- `audit/memory.rs` refs query (136-139): add deterministic `ORDER BY key ASC, id ASC` before `LIMIT 500`.
- Command: `cargo test --locked --lib dashboard::data` and `cargo test --locked --lib audit::memory`.

### Commit 7 — `fix(reuse): require reference currency in the reuse allowlist` (I-4) — depends on commit 6

#### src/db/query_state.rs (Modify)

- Scope: relocation home.
- Change: `pub(crate) enum RefStatus { Valid, Modified, Broken }` and `pub(crate) fn check_ref_status(index_conn, r, anchor)` move here from `src/service/mem_query.rs:81-121` verbatim (plus commit 6's ESCAPE/ORDER BY shape, and — r6 ledger correction, Round-6 Resolution Map B3 — the symbol arm's own `ORDER BY f.mtime DESC, s.qualname, s.file_path LIMIT 1`, added because the `qualname OR name` probe is a MULTI-INDEX OR whose served row otherwise follows rowid order between fresh and incremental builds). The helper is self-contained SQL over the index connection, so the move is mechanical and behavior-neutral.
- Rationale: `query/reuse` must not gain the crate's first query→service edge (zero `crate::service` uses exist under `src/query/`, `src/db/`, `src/extract/`, `walk.rs` — grep verified; `service/mem_query.rs:402,406` already calls `crate::query`). The db layer is the shared home both consumers already reach into.
- LOC budget: 380 + ~45 (the moved helper spans `mem_query.rs:81-121`, ~41 lines, plus import) → ~425 — this is the single number; the budget table row matches it (r3).

#### src/service/mem_query.rs (Modify)

- Change: delete the local `RefStatus`/`check_ref_status` and re-point `check_note_ref_status` and other callers at `crate::db::query_state::{RefStatus, check_ref_status}`; add the re-export if needed for test ergonomics. File shrinks (~-35), keeping headroom for commit 6's inline tests.

#### src/query/reuse/allowlist.rs (Modify)

- Scope: `load` (10-42), `ref_is_fresh` (44-55), tests mod (exists at 62).
- Change: `ref_is_fresh` gains an `anchor: i64` parameter and shape-branches (Technical Plan §5); refs are consumed verbatim (no `:` splitting — the display side never splits either). Path-shaped refs (contain `/`): require the exact `files` row (`SELECT 1 FROM files WHERE path = ?1`), `!staleness::file_changed`, and `crate::db::query_state::check_ref_status(...) == Valid`; non-path refs: `check_ref_status(...) == Valid` alone (its symbol arm resolves name/qualname against the anchor). The `!refs.is_empty() &&` gate at line 36 is kept verbatim — notes without refs stay accepted. No `AllowEntry` struct is introduced (the review's imagined one does not exist; `load` returns `HashSet<(String, String)>`).
- Data flow: after `eg build`, `file_changed` alone returns false for a long-stale file; the anchor comparison keeps the pair suppressed only while no referenced entity is newer than the note's anchor. The exact-files precondition makes the moved-file case fail closed instead of slipping through the helper's LIKE suffix arm.
- Tests: e2e in `tests/cli/reuse_allowlist.rs` (Create — registered as `mod reuse_allowlist;` in `tests/cli/main.rs`, r5; `tests/cli/reuse.rs` measures 477 and commit 7's three fixture-store tests would push it to ~520-530, hard-failing the hygiene gate, so they live in a dedicated file built on the same `tests/cli/common.rs` helpers — `temp_repo`/`write`/`eg` — that every reuse test there uses):
  1. `allowlist_note_requires_touch_after_referenced_file_changes` — `eg mem write reuse-allow/<pair> --refs <file>` at t0, touch the file's mtime forward, `eg build`, `eg reuse` reports the duplicate; `eg mem touch reuse-allow/<key>` then suppresses it.
  2. `moved_ref_does_not_suppress_through_suffix_match` — note referencing `src/old.rs`, file moved away, a `vendor/src/old.rs`-shaped file exists with an older mtime; the duplicate stays reported.
  3. `refless_allowlist_note_still_suppresses` — a `reuse-allow/` note with no `--refs` suppresses as today (pins the preserved contract; the no-refs pin needs a fixture store + QueryCtx that the pure unit mod does not have — round-1 council advisory adopted).
  4. Unit test in the existing allowlist mod (`src/query/reuse/allowlist.rs`): `symbol_ref_freshness_follows_check_ref_status` — an in-memory index connection built exactly like commit 6's tests (`Connection::open_in_memory` + `include_str!("../../../schema/index.sql")`) holding one symbol row, with `anchor` passed as plain integers (`t0` fresh, later `t1` modified); asserts the symbol branch's accept/reject follows `check_ref_status` directly (no store fixture needed — the branch is pure over the connection).
- Command: `cargo test --locked --test cli reuse_allowlist`, full `--test cli` reuse sections before commit.

### Commit 8 — `fix(reuse): filter min_confidence before judge and freshness` (I-3)

#### src/query/reuse.rs (Modify)

- Scope: `analyze_corpus_with` (258-…), insertion immediately before the `judge::apply` call (~line 293).
- Change: before judge and freshness, count candidates below `opts.min_confidence` and retain the rest in the `mut pool`; add the count into `ReuseCounts.below_threshold` and fold it into `total` at report construction.
- Safety argument (verified, r2 anchors): production `judge::apply` only rejects candidates and never assigns confidence (`judge.rs:41-89`; the `Low` literal at the file's `:324` is a test fixture), and freshness only demotes to `Low` (`freshness.rs:314-321`), so pre-filtering cannot lose candidates that would have been promoted; `for_hints()` forces `min_confidence = Low` (`opts.rs:85`) so hint detection is unaffected.

#### src/query/reuse/finalize.rs (Modify)

- Scope: threshold accounting (31-38).
- Change: rename the post-collection leftover count to `extra_below` (candidates demoted by judge/freshness after the collect-time filter) and report `below_threshold = report.counts.below_threshold + extra_below`; the retain itself stays (idempotent). No double counting: collect-time below-threshold candidates were removed before judge, and freshness-demoted candidates are the only new entries in this set.

#### tests/cli/reuse_phase5.rs (Modify) — NEW spy harness, not an existing one (r2)

- Change: add `reuse_min_confidence_medium_skips_judge_and_keeps_freshness_scope`:
  1. Create a judge stub script that appends one line per invocation to a count file; install it via the existing `EG_JUDGE_BIN` / `EG_JUDGE_MODEL` env hooks this file already exercises (the `.env(...)` install sites sit at `tests/cli/reuse_phase5.rs:41-42` and `:95-96`; the surrounding setup block starts at `:17`).
  2. Temp fixture repo with a Low-only duplicate pair; run `eg reuse --min-confidence medium --judge`.
  3. Assert the count file is empty (zero judge subprocess spawns), the summary line reports the below-threshold count, and freshness headers are still present; a control run at `--min-confidence low --judge` asserts the count file is non-empty (the spy actually works).
- Command: `cargo test --locked --test cli reuse_phase5` (filter: `min_confidence`).

### Commit 9 — `fix(reuse): deterministic coverage and judge ordering` (I-11)

#### src/query/reuse/coverage.rs (Modify)

- Scope: symbol filtering (39-47), `class_candidates` (72-107), new `#[cfg(test)] mod tests`.
- Change: `unresolved_callees` retain drops the `|| is_raw_client(...)` disjunct (coverage mode never runs D4, so raw-client retention there only pollutes D1L); `class_candidates` hoists the per-class prepared statement out of the loop (prepare once, re-bind per class — the ESCAPE'd LIKE already present stays).
- LOC budget: 306 + ~2 + inline tests ~60 → ~368.

#### src/query/reuse/finalize.rs (Modify)

- Scope: report sort (12-22).
- Change: append the two missing tie-breakers `canonical_file` then `canonical_line` (mirroring `coverage.rs:62-63`). The review's context lines named nonexistent `candidate_file`/`candidate_line` fields — real fields are `file`/`line` (already present in the chain); only the two additions are new.

#### src/query/reuse/judge.rs (Modify)

- Scope: the `source()` symbol lookup (`judge.rs:162-197`; the lookup SQL sits at 179-188).
- Change: `ORDER BY` becomes `line_start, qualname` before `LIMIT 1` (deterministic when `name` and `qualname` ties exist). File 354 + 1 → fine.

#### tests

- `coverage_raw_client_still_matches_user_defined_symbol` (coverage tests mod, renamed r6 per Round-6 Resolution Map B1 — the planned `coverage_raw_client_not_in_unresolved` asserted the opposite of the real defect, whose removed disjunct is unreachable dead code): index fixture reusing the existing unit-fixture pattern, pinning both directions of the retain — a user-defined `fetch` stays a D1L match through `unresolved_candidates` (true positive by design, the same reason D4 keeps raw clients on `all_callees`) while a stoplisted callee (`unwrap`) does not reach the pool; `finalize_sort_tie_breaks_on_canonical_file_and_line`: two constructed candidates with identical symbol/file/line and distinct canonical files sort deterministically.
- Command: `cargo test --locked --lib query::reuse`.

### Commit 10 — `perf(memory): restore the pending partial index and cache SQL` (I-10)

#### src/db/memory_fts.rs (Modify)

- Scope: `reconcile_fts_chunk` (133-199); new `#[cfg(test)] mod tests`.
- Change: replace the single OR-chained selector with a two-arm UNION ALL of subselects: arm A keeps `superseded_by IS NULL AND (expires_at = 0 OR expires_at > ?1) AND fts_version < 2`; arm B keeps the same predicates plus `fts_version >= 2 AND (id NOT IN (SELECT rowid FROM memories_fts) OR id NOT IN (SELECT rowid FROM memories_fts_v2))`; each arm orders by `id` and limits to the chunk size inside its subselect, the outer select re-orders by `id` and limits to the chunk size (arms are disjoint, so batch semantics are unchanged). The five per-row `conn.execute(format!(...))` calls become: two DELETE and two INSERT prepared statements built once per chunk with fixed table-name SQL strings (no `format!` interpolation), plus one `prepare_cached` UPDATE for `fts_version = 2`; bindings per row unchanged.
- API shape: `reconcile_fts_chunk(&Connection) -> Result<usize>` unchanged; callers untouched.
- Tests (new mod): `reconcile_uses_pending_index` — `EXPLAIN QUERY PLAN` over arm A reports `memories_pending`; `reconcile_repairs_missing_fts_rows` — insert rows with `fts_version = 0` and one row missing only from `memories_fts_v2`, run chunks until 0, assert both FTS tables carry all rows and `fts_version = 2`.
- Command: `cargo test --locked --lib db::memory_fts`.

#### src/dashboard/data.rs (Modify)

- Scope: `read_entries_cached` / `EntryCache` (127-156).
- Change: the cache stamp gains the `log_path` `PathBuf` as its first component (the rotated sibling is derived from `log_path`'s parent, so one path component suffices to disambiguate). Behavioral cache test dropped (round-1 rationale stands: structural change; the same-stamp different-content case is constructible only by hand-setting mtimes and is covered by the consumer suite).
- Command: `cargo test --locked --lib dashboard::data`.

### Commit 11 — `refactor(dashboard): extract journal ingest and bound line reads` (I-12 line bounds, r2 LOC extraction)

#### src/dashboard/data/ingest.rs (Create)

- Scope: new submodule of `data.rs` — the journal-ingest cluster, moved so `data.rs` returns under the 500-line hygiene gate (currently 499, the largest file in the repo; round-1 council finding B15).
- Change: move from `data.rs`: `discover_repos` (26-63), `retain_untracked` (66-84), `MAX_LINE_BYTES` + `live_event` (99-107), `generation_stamp` (109-122), `EntryCache`/`read_entries_cached`/`read_usage_entries` (127-163), `read_usage_entries_uncached` (165-184), `Tail`/`TailEvent`/`file_identity`/`poll` (221-318). `data.rs` keeps `mod ingest;` with re-exports preserving today's paths (`data::read_usage_entries`, `data::live_event`, `data::Tail`, and `super::discover_repos` / `super::retain_untracked` for `data/memories.rs` and `data/reuse.rs` — child-module visibility rules make the private re-exports work unchanged). `read_snippet` stays in `data.rs`.
- New behavior in the same commit: a capped line reader declared `pub(super)` in `ingest.rs` (the parent `data.rs` needs it for `read_snippet`, which stays behind — a private child item would not be visible to the parent, r3) — reads up to `MAX_LINE_BYTES` (256 KiB) searching for the newline; a fitting line returns, an oversized line is discarded through its newline and skipped, a torn tail stays unread exactly as `Tail` documents. Applied to `read_usage_entries_uncached`, `Tail::poll`'s read loop (301-310), and `read_snippet` (fn spans `data.rs:189-219` — the third reader of the same class; round-1 council advisory). `live_event`'s post-hoc length check stays as defense in depth; the doc-comment at `MAX_LINE_BYTES` gains the silent-skip contract sentence (oversized lines are dropped before parse and are not surfaced in any counter — documented limitation, observability follow-up candidate).
- LOC budget: data.rs 499 − ~200 moved + ~15 (re-exports + snippet rewire) → ~315; ingest.rs ~230 incl. the capped reader.
- Rationale: one extraction satisfies both the I-12 bound and the LOC gate; the reader is written once and used by all three call sites.

#### tests — src/dashboard/data/tests.rs (Create) plus existing inline mods where present

- `uncached_reader_skips_oversized_line` — temp file with a 300 KiB line followed by a valid event line; only the valid event parses.
- `capped_reader_accepts_line_at_exact_cap` — a line of exactly 256 KiB ending in a newline is accepted (pins the fits/discards boundary).
- `tail_poll_oversized_line_yields_reset_free_skip` — append an oversized line then a valid one; `Tail::poll` delivers only the valid line and keeps a sane offset.
- Existing data.rs test mods keep passing against the moved fns (import paths preserved by re-exports).
- Command: `cargo test --locked --lib dashboard::data`.

### Commit 12 — `fix(repo): canonicalize worktree pointers before layout checks` (I-12, part pointer spoof)

#### src/repo.rs (Modify)

- Scope: `worktree_main_root` (108-141); tests mod (exists at 169).
- Change: cap the `.git` pointer read at 4096 bytes (larger pointers are treated as malformed: `None`); after resolving the raw gitdir (absolute or root-relative), canonicalize it first and run the `worktrees` / `.git` segment checks on the canonical path; the final `main` canonicalization stays. Honest relative pointers and real worktrees are unaffected (verified behavior: canonicalizing `/victim/.git/worktrees/..` yields `/victim/.git`, whose parent segment is `.git`, not `worktrees`, so the spoof resolves `None`).
- API shape: `pub fn worktree_main_root(&Path) -> Option<PathBuf>` unchanged (the review diff targeted a nonexistent `detect_git_worktree` returning a tuple — rejected).
- Tests: `worktree_pointer_with_dotdot_after_worktrees_is_rejected` — build `victim/.git/` dir + `spoof/.git` file containing the spoof pointer (fixtures via `crate::db::test_support`); assert `worktree_main_root(&spoof)` is `None` (today it returns the victim path — reproduced this session); plus `oversized_pointer_is_rejected` (4 KiB + padding) and one honest-worktree positive case stays green.
- Command: `cargo test --locked --lib repo::`.

### Commit 13 — `fix(journal): strip control characters from agent names` (I-12, part ANSI injection) — r2 new commit

#### src/journal/agent.rs (Modify)

- Scope: `canonical_agent` (60-70) and its tests (add to the file's existing test surface; file is 233 lines, room for an inline test mod).
- Change: before the 32-character cap, filter the trimmed name to safe content: retain only characters that are neither Unicode control characters (C0/C1, including `ESC`) nor bidi/format characters (`0x202A..=0x202E`, `0x2066..=0x2069`, `0x200B..=0x200F`, `0xFEFF`) — the format characters pass `is_control()` but would still flip terminal rendering or spoof attribution in the raw-mode renderer and the web tags (r3). An escape sequence collapses to its harmless printable remainder. Applied on the alias-miss path and every path that feeds `pending.agent` (`journal.rs:149-154`), so MCP `clientInfo.name` and `ENGRAM_AGENT` values can no longer carry terminal escape payloads into `eg monitor`'s raw-mode renderer (`monitor/render.rs:421-426`, which only truncates to 9 chars). The commit message states the residual explicitly: journal lines already written before this commit are not rewritten (same-user data, matching the worktree-pointer residual); the filter protects new writes only.
- API shape: `fn canonical_agent(&str) -> Option<String>` unchanged.
- Tests: `ansi_escape_bytes_are_stripped_from_agent_names` — inputs containing `ESC [ 31 m`, C0 bytes, a CR/LF, a bidi override `U+202E`, and a zero-width space `U+200B` yield a printable-only, <= 32-char canonical name (the bidi/format ranges added in r3 are pinned by the same test, r4); the alias table path is unaffected.
- Command: `cargo test --locked --lib journal::agent`.

### Commit 14 — `fix(root): route root discovery through RepoRef::resolve` (I-9)

#### src/mcp.rs (Modify) + src/mcp/tests.rs (Create, out-of-line)

- Scope: `root_from_path` (350-352); tests live in a new out-of-line `src/mcp/tests.rs` (the file has no test module today — grep verified — and sits at 480 LOC, so the new tests must not push it over the cap).
- Change: body becomes `crate::repo::RepoRef::resolve(Some(start)).ok().map(|r| r.root().to_path_buf())`. `.engram`-at-start now wins over the enclosing git root (fixes the inverted priority inside monorepo sub-packages); filesystem-root inputs map to `None` as before.
- Tests (tests.rs): `mcp_root_from_path_respects_nested_engram_store_inside_git_monorepo` — temp outer repo with `.git/` and `outer/packages/sub/.engram/`; assert `root_from_path(&sub)` equals the canonicalized sub path (canonicalization comes from `RepoRef::from_root`, `repo.rs:63-64` — accepted, documented). Fixtures via `crate::db::test_support`.
- Command: `cargo test --locked --lib mcp::`.

#### src/audit.rs (Modify)

- Scope: `execute` local-mode root (44-50); tests mod exists.
- Change: resolve via `RepoRef::resolve(root_arg)` with the r2-pinned error rule: when `root_arg.is_some()`, resolve errors propagate unchanged (`--root /` and `--root not-a-dir` keep exit 2 per `docs/health.md:33`; every explicit-root discovery failure is `Usage`-typed — `walk.rs:44`, `walk.rs:69`); when no explicit root is given, every resolution failure maps to the existing `NoResults` "audit: no repository here; run eg audit --global" message (exit 1) — no-root failures may be `Usage` (`walk.rs:55`) or `Internal` (cwd unreadable, `walk.rs:53`), and both must keep today's exit 1.
- Tests: extend the audit tests mod with the mapping rule asserted at the resolution level (`RepoRef::resolve(Some(subdir))` roots at the git repo); `tests/cli/audit.rs` stays green end-to-end.
- Command: `cargo test --locked --lib audit::tests` (mapping rule) and `cargo test --locked --test cli audit` before commit.

#### src/dashboard/monitor/mod.rs (Modify)

- Scope: `resolve_monitor_log_path` (347-368).
- Change: in the no-explicit-root local branch, after the existing cwd probe fails, resolve the enclosing repository via `RepoRef::resolve(None)` and probe its `usage_log_path` before falling back to the global journal — running `eg monitor --local` from a subdirectory no longer silently reads the global log.
- Tests: the cwd branch needs process-wide cwd mutation, which parallel tests cannot do safely — known verification gap, covered indirectly by the `RepoRef::resolve` tests of this commit; the explicit-root branch gains one assertion in the monitor tests mod (mod exists at 370): `resolve_monitor_log_path(Some(explicit_root), true)` returns the repo's journal path.
- Command: `cargo test --locked --lib dashboard::monitor`.

### Commit 15 — `fix(dashboard): exclude ephemeral stores from touch and dedupe startup prune` (I-16a, N-1, I-8 ephemeral fold)

#### src/journal/liveness.rs (Modify)

- Scope: `entry_is_ephemeral` (115-126); tests mod exists (attribute at 164, `mod tests` at 165).
- Change: resolve the effective root as `crate::repo::cached_worktree_main_root(Path::new(repo)).unwrap_or(repo)` before the `judgeable_root` + temp-prefix checks. A linked worktree under the OS temp tree folds to its main repository and survives the prune; ordinary repos are unchanged (cache returns `None` after one stat).
- Rationale: the review's diff reads `entry["worktree_main_root"]`, a field the journal writer never writes (`journal.rs:135-159` writes `worktree` only) — provable no-op; `summary.rs:100-105` already uses exactly this resolver for the same purpose.
- Tests (existing liveness tests mod): `worktree_under_tmp_folds_to_main_root` — a worktree directory under the temp tree whose `.git` pointer resolves to a real main repo returns `entry_is_ephemeral == false` (fixture pattern from the repo.rs tests); a plain `/tmp` repo entry stays ephemeral.

#### src/dashboard/server.rs (Modify)

- Scope: startup (line 82) and `handle_client` dispatch (386-388 — r2 anchors: the duplicate prune calls sit at `server.rs:82` and `:93`).
- Change: delete the first `prune_stale_entries_all` call; the `:93` call and the ephemeral prune remain. Pass the existing `global` flag (`handle_client`'s `is_global`) into the touch handler.

#### src/dashboard/server/touch.rs (Modify)

- Scope: `handle_memory_touch` signature (9-14), entry read (54), and — r6 ledger correction per Round-6 Resolution Map B3 — the current-rev picker probe (~90-104).
- Change: signature gains `global: bool`; when `global`, filter the parsed entries through `!crate::journal::liveness::entry_is_ephemeral(entry)` before `resolve_store`/`store_ids` — global mode never offers a `/tmp` store for mutation, matching what every read endpoint already shows. The r6-corrected picker probe mirrors the CLI touch's inactive-revision contract in SQL: `superseded_by IS NULL AND (expires_at = 0 OR expires_at > ?2) ORDER BY rev DESC LIMIT 1`, so an expired revision can never pass the probe and be re-anchored by `memory_touch_if_rev` (whose own UPDATE gate the same correction maps into Commit 4); after an expired-key re-write two rows of one key can both carry `superseded_by IS NULL`, and the highest revision wins deterministically.
- API shape: `handle_memory_touch(&mut TcpStream, Option<&Path>, &Path, bool, &HttpRequest)`.
- Tests: extend `tests/cli/dashboard/touch.rs` (a module of the `cli` target, `dashboard.rs:451`) — seed the global journal with one `/tmp` store entry and one real store; assert the touch 404 payload's `available_stores` omits the `/tmp` store in global mode and includes it in explicit-root local mode. The r6 ledger also carries the shipped e2e `dashboard_touch_rejects_expired_key` (`tests/cli/dashboard/touch.rs`, ~line 49): a session note whose 1-second TTL has passed 404s as `unknown key` instead of re-anchoring the dead row with a fresh `anchored_at`; mutation-verified to fail with a 200 re-anchor on the pre-fix pipeline.
- Command: `cargo test --locked --test cli dashboard::touch`.

### Commit 16 — `fix(monitor): unify repo filters and hide ephemeral entries in global mode` (I-16b complete) — depends on commit 14

#### src/dashboard/monitor/mod.rs (Modify)

- Scope: `is_event_matched` (314-333) and both ingest loops (236-237 TUI, 305-309 linear).
- Change: two coupled edits:
  1. The unified predicate adopts the TUI's stricter name resolution — `repo_name` that is absent or empty falls back to the basename of `repo`, then `UNATTRIBUTED` — making the linear predicate the single source of truth, and gains an `exclude_ephemeral: bool` term implemented via `crate::journal::liveness::entry_is_ephemeral`.
  2. Both ingest loops pass `exclude_ephemeral = true` exactly when the monitor is reading the global journal (the mode `resolve_monitor_log_path` signals by returning no repo root): a `/tmp` scratch entry disappears from `eg monitor` in global mode while `--local` sessions are untouched.
- Rationale: review claim covered the monitor half explicitly (leak of `/tmp` stores onto the monitor screen); round 1 dropped it — restored here (r2 B4).

#### src/dashboard/monitor/state.rs (Modify)

- Scope: `TuiState::matches_filter` (127-154).
- Change: delegate to `super::is_event_matched(val, self.repo_filter.as_deref(), self.errors_only, /* exclude_ephemeral from the session mode */)`; the duplicated body is deleted. The session's mode flag is set once alongside the existing constructor arguments (`TuiState::new`, `mod.rs:110`).
- Tests (monitor tests mod, 370): `monitor_linear_and_tui_share_identical_event_filter` — one entry with `repo` only (no `repo_name`) and one with `repo_name` must match `--repo <basename>` identically through both the free function and a constructed `TuiState`; an errors-only entry with `exit_code < 3` is rejected by both; `global_monitor_hides_tmp_entries` — a `/tmp`-rooted entry is invisible in global mode through both paths and visible in local mode.
- Command: `cargo test --locked --lib dashboard::monitor`.

### Commit 17 — `fix(dashboard): harden HTTP host checks and connection caps` (N-2, I-12 thread/request caps)

#### src/dashboard/server/http.rs (Modify)

- Scope: `host_allowed` (83-93), request-line read (~22), tests (new inline mod).
- Change:
  1. Hostname extraction becomes: a bracketed host yields the text between the brackets; otherwise the text before the first `:`. Bracket-less IPv6 forms (`::1`, `::1:8080`) yield an empty or ambiguous extraction and stay rejected — RFC 7230 requires brackets for IPv6 literals, and today's behavior already rejects bare `::1` (verified `http.rs:87-92`), so the allowed set is genuinely unchanged while `[::1]:port` starts working.
  2. The initial request-line `read_line` is bounded by `MAX_HEADER_BYTES` (8 KiB): a line exceeding the cap aborts parsing (return `None`) instead of growing without limit — the doc-comment at `http.rs:1` claims bounded parsing; this makes it true for the first line too.
- Tests: `host_allowed_accepts_loopback_forms` — `[::1]:8080`, `127.0.0.1:8080`, `localhost:3000`, `0.0.0.0:80`, `localhost` accepted; `evil.example:80`, `evil.example`, `[2001:db8::1]:443`, `::1`, `::1:8080` rejected; `request_line_over_cap_is_rejected` — an 8 KiB+ first line yields no request.

#### src/dashboard/server.rs (Modify)

- Scope: accept loop (102-119), HEAD `/events` header (426), guard pattern (437-447).
- Change: add `static ACTIVE_CONNS: AtomicUsize` with `const MAX_CONCURRENT_CONNS: usize = 64` and a `ConnGuard` Drop type (same pattern as `SSE_CLIENTS`/`SseGuard`): increment before `thread::spawn`, decrement on thread end; beyond the cap answer `503 Service Unavailable` + close without spawning, and emit one rate-limited stderr line (`eg dashboard: connection cap reached (64)`) so saturation is visible — the file's own contract says one stderr line per request is the only way to see a stuck route. Delete `Access-Control-Allow-Origin: *` from the HEAD `/events` header (GET `/events` and `send_response` never send it).
- LOC budget: 447 + ~30 → ~477 (tests live in the cli target, not inline).
- Tests: `tests/cli/dashboard/security.rs` gains `connection_cap_rejects_overflow` — 64 holder sockets opened without sending any bytes (their threads block on the header read, keeping the counter at the cap), the 65th request draws 503; the raw-HTTP harness in that file already speaks TcpStream.
- Command: `cargo test --locked --lib dashboard::server` and `cargo test --locked --test cli dashboard::security`.

### Commit 18 — `fix(dashboard): correct memory store click filter and keyboard access` (I-13, I-15)

#### src/dashboard/assets/app.js (Modify)

- Scope: `filterMemoryByRepo` (912-931), `renderReposTable` (518-607), `renderReuseAgentsTable` (764-790), `renderMemoryAgentsTable` (881-909), plus the memory repos table rows (830-839); module state near the top.
- Change:
  1. `filterMemoryByRepo(storeName)`: find the first `<option>` of `#mem-store-filter` whose value or text equals the requested name (options are built with `value = store_id`, `textContent = store` at 1331-1339; rows pass `rData.display || rName` at 838); select its value; a second click of the same store returns to `'ALL'` — the toggle compares the matched option value, not the raw label (comparing the label would never match after the fix); no match falls back to `'ALL'`; never append a ghost option; call `renderMemoryList()` on every path. (No `'(global)'` branch exists in this function — the review diff invented one; nothing to remove, r2.)
  2. Expansion persistence: a module-level `Set` keyed by repo display label records expanded worktree rows; `renderReposTable` re-applies `aria-expanded` and the `wt-open` class from the set when rebuilding rows (SSE refresh no longer collapses open rows).
  3. Worktree-row filter target: the repository label inside the first cell becomes its own focusable filter trigger (`role="button"`, `tabindex="0"`, Enter/Space + click call `filterByRepo(label)`); the label's handlers call `stopPropagation()` (and `preventDefault()` for Space) so the row's toggle does not also fire; the row's own click/keydown keeps toggling worktrees.
  4. `aria-label` is removed from every `<tr role="row">` filter target across all four tables (five sites: telemetry repo rows at 543/546, telemetry agent rows at 657, reuse repo rows at 720, memory repo rows at 835) — screen readers read the cell contents. Compensating feedback closes the gap this removal would open: each of the four filter handlers gains one `announce('Filtered by <label>')` call (`filterByRepo` ~1121-1127, `filterByAgent` ~691, `filterReuseByRepo` ~793-812, `filterMemoryByRepo` ~912-931), mirroring the existing `announce` pattern at `app.js:599` — no filter handler announces anything today (verified by grep: every `announce(` call site is a toggle/reset/selection/verify/stream event), so without this the removal would delete the only non-visual action name SR users hear.
  5. Both agent tables: each row gains `tabindex="0"` and an Enter/Space keydown invoking the same handler as its click (pattern at `app.js:602, 605`).
- Tests: `tests/cli/dashboard/assets.rs` (Create) + `mod assets;` in `tests/cli/dashboard.rs` (mod block at 448-451): a Rust guard reading the asset via `include_str!("../../../src/dashboard/assets/app.js")` asserts the affordances exist (the expansion Set is read during render, `tabindex` present in both agent-table builders, an `announce('Filtered by` call in each of the four filter handlers, `filterMemoryByRepo` contains no `appendChild`) and that `clickable-row` never returns (grep-count 0). The behavioral check stays the documented manual pass (no JS harness exists in the repo — declared limit, round-1 Risks): click a Memory Store row selects the store-id option and shows that store's count; second click returns to All; expanded rows survive one SSE cycle; Tab reaches repo rows, agent rows, and the label target; Enter activates each without toggling the row.
- Command: `cargo test --locked --test cli dashboard::assets` and `bash tests/scripts/hygiene_check.sh`.

### Commit 19 — `fix(dashboard): mark expired notes as not current` (I-14) — depends on commit 6

#### src/dashboard/data/memories.rs (Modify)

- Scope: `collect_store_notes` SQL construction (340-353); tests move to a submodule (file is 490 LOC — round-1 finding B15).
- Change: `is_current` becomes `(superseded_by IS NULL AND <expiry>)` where `<expiry>` is `(expires_at = 0 OR expires_at > ?1)` when the store has the `expires_at` column (guard via the file-local `column_present` at 387-396, mirroring the `anchored_at` guard at 342-346 — the shared consolidation happens later in commit 23, so this commit introduces no cross-commit dependency) and the constant `1` when it does not (a pre-2026-11-09 v1 store has no TTL concept, so nothing can be expired). Parameters bind per branch: `query_map([now], ...)` for the guarded SQL, `query_map([], ...)` for the v1 fallback — an unconditional bind would fail `InvalidParameterCount` exactly on the stores the guard protects (r2 B2). The SELECT list keeps its exact column order — the decode closure's indexes (10 = anchor, 11 = is_current) do not move, unlike the review's diff. `crate::db::unix_now()` supplies `now`.
- LOC budget: 490 − ~95 (existing tests mod moved) + ~10 → ~405.

#### src/dashboard/data/memories/tests.rs (Create)

- Change: the existing tests mod body moves here unchanged, plus two new tests:
  1. `dashboard_memories_marks_expired_session_notes_as_not_current` — temp store via `open_memory_store`, one live session note and one with `expires_at` in the past; `collect_memories(Some(root), &[])` marks the expired one `is_current == false` and the live one `true`.
  2. `v1_store_without_expires_column_still_collects` — the fixture is built deterministically by executing the text of `schema/memory.sql` with the `expires_at` column definition filtered out (no `DROP COLUMN` dependency; a pre-migration `.db` fixture does not exist under `tests/fixtures/memory/` — verified); `collect_memories` returns rows with `is_current` per the supersession predicate alone and no binding error (r3 B2 follow-up: single mechanism, no implementer choice).
- Command: `cargo test --locked --lib dashboard::data::memories`.

### Commit 20 — `fix(journal): price only successful calls in saved tokens` (I-7)

#### src/journal/render.rs (Modify)

- Scope: `ToolAgg` (31+), `fold_tool` (~112-150), `savings_basis` (~232), `calculate_telemetry_totals` (~405-421), `aggregate_global_usage` (~288-318); tests in `src/journal/render/tests.rs` (exists).
- Change: `ToolAgg` gains `ok_calls`; `fold_tool` counts `ExitClass::Ok` into `ok_calls` (Empty/Ambiguous neither priced nor errored) and accumulates `total_shown`/`total_results` only for Ok; `savings_basis` and `calculate_telemetry_totals` pass `agg.ok_calls` instead of `calls - errors`. In `aggregate_global_usage`, fold linked worktrees into their main repository via `crate::repo::cached_worktree_main_root` and skip `entry_is_ephemeral` entries — the same truthfulness rules the dashboard summary applies.
- API shape: private fns only; both consumers of the totals are the CLI `eg usage` renderer and the journal KPI — no external contract change beyond the corrected numbers.
- LOC budget: 471 + ~22 → ~493.
- Tests (render/tests.rs): `usage_cli_does_not_price_empty_or_ambiguous_exit_codes` (exit 1 + exit 2 lines → `saved_tokens == 0`, fails before the fix, passes after) and `global_usage_folds_worktrees_and_drops_ephemeral` (a worktree entry under `/tmp` whose main repo is a temp real repo contributes under the main repo and never as its own row).
- Command: `cargo test --locked --lib journal::render`.

#### CHANGELOG.md (Modify)

- Entry under `## [Unreleased]`: `eg usage` savings now count only successful calls (exit 0); totals for sessions with empty-result or ambiguous calls decrease accordingly.

### Commit 21 — `fix(memory): detect JWTs embedded in JSON strings and assignments` (S-3)

#### src/memory/secret.rs (Modify)

- Scope: `has_jwt` (115-128); tests mod (exists at 189).
- Change: scan with `text.match_indices("eyJ")`, gate each hit through the existing `boundary(text, idx)` helper (used by `has_slack_token` at 139-140), collect the token as the maximal run of base64url characters and dots from the hit, and apply the unchanged three-segment rule (>= 3 parts, each >= 10 base64url chars). Define the base64url character class inline (ASCII alphanumeric, `-`, `_`).
- Rationale: the review's diff referenced an undefined `is_b64url` helper and left `parts` as a non-`Vec` iterator in a `len()`/`iter()` pair — both corrected in the description above; the existing negative case (`"eyJhbGciOi"` inside `accepts_short_prefixes`) still returns false because the short run never satisfies the segment rule.
- Tests: `jwt_detected_in_json_string_and_env_assignment` (`"token": "eyJ…"` with a valid three-segment shape, `JWT=eyJ…`) and the preserved negatives (`eyJ`-only, two-segment, short segments, mid-word false hit).
- Command: `cargo test --locked --lib memory::secret`.

### Commit 22 — `fix(extract): stop macro scan on UTF-8 boundary at the cap` (S-4) and `fix(dashboard): key panel discovery by repo path` (I-8 collision)

#### src/extract/walk/rust.rs (Modify)

- Scope: `scan_macro_call_names` (200-202); tests.
- Change: clamp the scan end to `min(raw.len(), MAX_MACRO_TOKEN_TEXT)` and walk back to the nearest `is_char_boundary` (always terminates at 0; at most 3 bytes back) — a multi-byte straddle no longer falls back to scanning the entire macro body.
- Tests: this file has no test module (the `cfg(test)` occurrences at 100-108 are scanner input strings, not a test mod — verified by grep for `mod tests`, zero hits); add a new inline `#[cfg(test)] mod tests`: `macro_scan_caps_at_char_boundary` — a synthetic macro body of multi-byte characters longer than 64 KiB; assert the scanner returns without panic and respects `MAX_MACRO_CALLS_PER_SITE`.
- LOC budget: 424 + ~5 + ~35 tests → ~464.
- Command: `cargo test --locked --lib extract::walk::rust`.

#### src/dashboard/data/ingest.rs (Modify) — the discover_repos re-key lands in its new home

- Scope: `discover_repos` + `retain_untracked` (moved here by commit 11).
- Change: the map key becomes the repo path string (journal `repo` values are canonical); candidates are collected first (explicit root short-circuit, then entry order, then cwd — order preserved) and deduplicated by path; display names are assigned deterministically over the sorted path set via a self-contained helper `display_name_for(path, taken_names) -> String`: basename, and on collision `parent-dir/basename`, escalating one ancestor per collision until unique (keeps nesting <= 3 and cyclomatic <= 10 per AGENTS.md §3). Consumers keep emitting the display name and use the path as identity; `retain_untracked` keys its security errors by display name as today.
- Rationale: `HashMap<String, PathBuf>` keyed by basename silently drops the second `/work/a/app` vs `/work/b/app`. Label uniqueness is load-bearing for commit 18's expansion `Set` (noted in both commit messages).
- Tests (data/tests.rs): `discover_repos_keeps_same_basename_repos` — two journal entries with distinct repo paths sharing a basename, both with `.engram/<db_rel>` present; assert two map entries with distinct keys and disambiguated display names.
- Command: `cargo test --locked --lib dashboard::data`.

### Commit 23 — `refactor(db): consolidate duplicated helpers` (S-1 dedup) — depends on commits 6, 9, 19

#### src/db/query_state.rs (Modify)

- Change: `column_present` promoted from `pub(super)` to `pub(crate)` (body unchanged, `pragma_table_info` variant returning `Result<bool>`). (Commit 7 already moved `check_ref_status`/`RefStatus` here.)

#### src/dashboard/data/memories.rs and src/audit/memory.rs (Modify)

- Change: delete both private `column_present` copies (`memories.rs:387-396` — now living in the tests-externalized file layout, and `audit/memory.rs:196-205`); call sites use `crate::db::query_state::column_present(conn, table, column).unwrap_or(false)` — identical swallow-and-false semantics.

#### src/query/reuse/guards.rs, coverage.rs, proposal.rs, target.rs (Modify)

- Change: `language_family` keeps its single definition in `guards.rs` (already `pub(super)` within `query::reuse`, reachable as `super::guards::language_family` per the reconciliation); the duplicate definitions in `coverage.rs` and `proposal.rs` are deleted and their call sites re-pointed. `escape_like`/`like_escape` duplicates in `coverage.rs:160` and `target.rs:325` are deleted in favor of `crate::db::like_escape` (introduced in commit 6) — these two files gain their first downward query→db edges; downward, no cycle.
- Rationale: behavior-neutral deduplication; the gate is the unchanged existing suite (578 tests) plus the new tests from commits 5-9, 19, 22.
- Command: `cargo test --locked --lib` (full lib run once — shared components touched).

### Commit 24 — `fix(surface): align CLI, MCP, and docs contracts` (I-17, S-5, N-3)

#### src/service/args.rs (Modify)

- Scope: reuse arg parse (257), mem_query kind/scope (314-315).
- Change: MCP `reuse` `min_confidence` defaults to `"low"` (matching the CLI default at `cli.rs:144`) instead of `unwrap_or_default()`; MCP `mem_query` passes `"mem query"` into `parse_kind_filter` and `parse_scope_filter` so error prefixes match the CLI byte-for-byte (`parse_time_bound`/`validate_window` at 299-307 already do).

#### src/cli.rs (Modify)

- Scope: `Mem` variant doc-comment (~230-231).
- Change: the action list gains `touch` and `reindex` (write/query/get/delete/export/import/gc already listed).

#### docs/health.md (Modify)

- Change: the heartbeat write command (line 115) uses the positional key `health/<YYYY-MM-DD>` instead of the nonexistent `--key` flag.

#### docs/cli-reference.md (Modify)

- Change: the `--all-tools` line states 20 tools with the 7 extended memory tools `mem_touch`, `mem_get`, `mem_delete`, `mem_gc`, `mem_export`, `mem_import`, `mem_reindex` (matches `TOOL_SPECS` and `docs/agent-integration.md:120`).

#### README.md (Modify)

- Change: the maintenance-commands line (155 — the section heading sits at 153) gains `touch|reindex`; the MCP footer example (211) becomes the current format — latency, `N items`, and `~T tokens saved vs file reads` per `format_footer` (`format.rs:72-81`).

#### docs/agent-integration.md (Modify)

- Change: the footer example (131) updated to the same current format (N-3).

#### tests/cli/packaging.rs (Modify — extends the file commit 1 created) and tests/mcp/parity.rs (Modify)

- Change: packaging.rs gains `health_doc_and_cli_reference_match_cli_and_mcp_contracts` — asserts `docs/health.md` contains no `--key ` usage and `docs/cli-reference.md` states the tool count equal to `engram::service::TOOL_SPECS.len()` (const, re-exported via `service`; the review's `crate::service::tool_specs(true)` API does not exist). The test lives beside commit 1's guard instead of in `tests/cli/p8_impact.rs` (476 lines — appending here would push it past ~488, r5) — `p8_impact.rs` stays untouched by this wave. `tests/mcp/parity.rs` gains one case invoking `mem_query` with an invalid `kind` and asserting the error text carries the `mem query` prefix (S-5 regression).
- Command: `cargo test --locked --test cli health_doc` and `cargo test --locked --test mcp parity`.

#### CHANGELOG.md (Modify)

- Entry under `## [Unreleased]`: MCP reuse default confidence and error-prefix parity; health.md automation prompt corrected; docs counts aligned. Finalize the section for the wave.

### Dependency Order

```
1  packaging include + review baseline relocation
2  audit baseline + p95          (creates collect tests file)
3  audit tool taxonomy          (after 2, same file + tests file)
4  memory lifecycle + surface docs + CHANGELOG Unreleased
5  unicode tokens + epoch bump  (CHANGELOG entry)
6  LIKE escape + ORDER BY       (introduces db::like_escape)
7  allowlist anchor + check_ref_status -> db   (after 6)
8  reuse pipeline order
9  coverage/judge ordering      (before 23 — coverage.rs touched)
10 FTS reconcile + cache key
11 ingest extraction + bounded line reads     (after 10 — same fns)
12 worktree pointer hardening
13 journal agent ANSI sanitize                (NEW in r2)
14 RepoRef::resolve parity
15 touch ephemeral + prune dedup + liveness fold (after 12/14 for the resolver)
16 monitor filter parity + monitor ephemeral   (after 14 — same mod.rs)
17 HTTP hardening (host, ACAO, caps)
18 dashboard assets a11y
19 memories is_current                        (after 6 — shares memories.rs)
20 usage pricing + CHANGELOG
21 secret JWT
22 macro scan + discover_repos re-key (in ingest.rs — after 11)
23 helper consolidation                       (after 6, 9, 19)
24 CLI/MCP/docs parity + CHANGELOG finalize   (last — full-surface sync)
```

Hard dependencies: 3 after 2 (same files); 7 after 6; 11 after 10; 15 after 12/14; 16 after 14; 19 after 6; 22 after 11; 23 after 6, 9, 19. Everything else is ordered for file-cluster coherence and review convenience, not correctness.

### Per-file LOC Budget (hygiene gate: hard fail above 500)

| File | Today | Δ | Result | Tests home |
|---|---|---|---|---|
| src/dashboard/data.rs | 499 | −~200 moved, +~15 | ~315 | data/tests.rs (Create) |
| src/dashboard/data/ingest.rs | — | +~230 | ~230 | shared data/tests.rs |
| src/audit/collect.rs | 464 | −~28, +~6 | ~442 | collect/tests.rs (Create) |
| src/db/memory_write.rs | 431 | +~28 | ~459 | memory_write/tests.rs (Create) |
| src/dashboard/data/reuse.rs | 498 | +~4 SQL shape, −~60 (both inline test modules at 437-438 and 478 moved out) | ~440 | reuse/tests.rs (Create, commit 6 — r3/r5) |
| src/dashboard/data/memories.rs | 490 | −~95, +~10 | ~405 | memories/tests.rs (Create, moves existing mod) |
| src/mcp.rs | 480 | +~2 | ~482 | mcp/tests.rs (Create) |
| src/journal/render.rs | 471 | +~22 | ~493 | render/tests.rs (exists) |
| src/extract/walk/rust.rs | 424 | +~40 | ~464 | inline (new mod) |
| src/db.rs | 459 | +~14 | ~473 | existing |
| src/db/query_state.rs | 380 | +~45 (ref-status move from mem_query.rs:81-121 + import) | ~425 | existing db tests family |
| src/dashboard/server.rs | 447 | +~30 | ~477 | tests/cli/dashboard/* |
| src/dashboard/monitor/mod.rs | 412 | +~10 | ~422 | inline (370) |
| src/query/reuse/coverage.rs | 306 | +~62 | ~368 | inline (new mod) |
| src/service/mem_query.rs | 422 | −~41 (commit 7 move), tests externalized | ~390 | mem_query/tests.rs (Create) |
| src/query/reuse.rs | 422 | +~10 (commit 8) | ~432 | existing |
| src/cli.rs | 433 | +~3 (commit 24 doc-comment) | ~436 | existing |
| src/service/args.rs | 374 | +~4 (commit 24) | ~378 | existing |
| others touched (src side) | — | small | < 440 each | as listed |
| tests/cli/reuse.rs | 477 | 0 — commit 7's e2e moved out (would have been ~520-530, hard-failing the gate) | ~477 | — (r5) |
| tests/cli/reuse_allowlist.rs | — | +~130 (commit 7's three e2e tests over the common.rs helpers) | ~130 | itself (Create, r5) |
| tests/cli/p8_impact.rs | 476 | 0 — commit 1/24 tests moved out (commit 1's guard alone would have reached ~491-501) | ~476 | — (r5) |
| tests/cli/packaging.rs | — | +~70 (commit 1 packaging guard + commit 24 surface tests) | ~70 | itself (Create, r5) |
| tests/cli/dashboard.rs | 451 | +~2 (`mod assets;`) | ~453 | dashboard/assets.rs (Create, commit 18) |
| tests/cli/main.rs | 31 | +2 (`mod packaging;`, `mod reuse_allowlist;`) | ~33 | — (r5) |

All test fixtures in new test files use `crate::db::test_support::{temp_root, write_file, append_file, make_dir_all, remove_tree}` exclusively — inline `fs::write`/`create_dir_all` outside the allowlisted files fails `tests/scripts/write_path_check.sh` (helpers verified at `src/db.rs:376-424`).

### Verification Plan

- Per commit, targeted: `cargo fmt --all -- --check`, `cargo clippy --all-targets -- -D warnings`, plus the commit's named test command from the ledger above. Test targets are exactly the four cargo test targets (`cli`, `mcp`, `fixtures`, `reuse_bench` — verified via `cargo metadata --locked --no-deps` this session) plus `--lib` unit filters.
- Once after commit 23 (shared components): `cargo test --locked --lib`.
- Final pre-merge gate, exactly once after commit 24 (per AGENTS.md §7, full-suite runs reserved for the final gate):

```
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test --locked
bash tests/scripts/allowlist_check.sh Cargo.lock
bash tests/scripts/write_path_check.sh
bash tests/scripts/hygiene_check.sh
```

Pass condition: all six green, test count >= 578 + the new tests introduced by this plan, zero new clippy warnings, allowlist unchanged (no dependency changes in this plan), and no `.rs` file above 500 lines.

- Spec self-check (r3, re-verified each revision): this design document itself must stay hygiene-clean — `bash tests/scripts/hygiene_check.sh docs/designs/2026-11-14_review-remediation.md` exits 0 (`docs/designs/*` is not in the exclusion list, so the file carries no workstation-path literals).

- Documentation-only gate for commit 24's doc hunks: validate every documented `eg ...` command against `eg <subcommand> --help`; check relative Markdown links (including the relocated baseline reference at the top of this spec); `git diff --check`.

## Risks and Limits

- **Canonicalization drift (commit 14)**: `RepoRef::from_root` canonicalizes roots (`repo.rs:63-64`) while today's `mcp.rs` fallback returns non-canonical paths; journal `repo` values are canonical by writer contract, so keying (commit 22) and comparisons (`same_root`) stay consistent — the risk is symlinked workspaces changing displayed paths, accepted and documented in commit 14.
- **FTS rebuild window (commit 5)**: the first write-open per store after the epoch bump runs the rebuild to completion inside that call — latency proportional to store size; "chunked" bounds each transaction, not the total call time. Reads degrade to the frozen legacy index meanwhile (`src/db/memory_fts.rs:57-58` → `src/memory/guard.rs:106-108` → `src/db/memory_query.rs:125-129` (direct legacy/v2 gate: `src/service/mem_ops.rs:137-141`)) and the rebuild is crash-safe (completeness marker stays 0 until done). Rollback re-heals in the opposite direction.
- **Touch TTL shortening (commit 4)**: re-asserting a session note without `--ttl` applies the 7-day default (Technical Plan §2); documented in the same commit across memory-store.md, cli-reference, SKILL.md, agent-integration, and CHANGELOG. Mitigated by `docs/health.md`'s heartbeat always passing `--ttl`.
- **Worktree pointer residual (commit 12)**: canonicalize-first closes the `..` spoof, but a free-form pointer naming another repository's real worktree path still nominates that main root; the journal is same-user data (no privilege boundary), so this is an attribution-hygiene residual, not a privilege boundary — back-pointer verification (`<gitdir>/gitdir` must point back at the reading repo) is recorded as a future candidate, not this wave.
- **Behavior-visible dashboard changes (commits 18, 22)**: store/repo display labels may gain `parent/basename` disambiguation; the manual UI pass in commit 18 is the check, since no JS harness exists — a known, stated verification limit backed by the Rust asset text-guard.
- **`eg usage` totals shift (commit 20)**: published savings numbers drop by the previously inflated exit-1/2 contribution; the CHANGELOG entry in the same commit states it.

## Revision History

- r1 (2026-11-14): initial spec, 23 commits. Rejected by the 12-persona council round 1 with 21 blocking findings (token-equality test that cannot pass; unconditional SQL binding breaking v1 stores; promised-but-absent ANSI and monitor hunks; allowlist probe ambiguity; judge-spy harness misreference; `::1` mechanism-vs-test contradiction; missing FTS epoch migration; false classify claim; commit-18/22 helper cycle; 500-LOC gate violations; query→service edge; missing CHANGELOG and touch docs; impossible audit error mapping; review.md hygiene conflict; four nonexistent test targets).
- r2 (2026-11-14): 24 commits, all 21 round-1 blocking findings resolved (see §Round-2 Resolution Map), adopted advisories folded in (test_support fixtures, boundary tests, supersede-chain preservation, anchors corrected, RFC 7230 Host decision, LOC budget table, ingest extraction).
- r3 (2026-11-14): all 6 round-2 blocking findings resolved (see §Round-3 Resolution Map) — reuse.rs LOC plan (tests externalized in commit 6), include_str depth corrected for the out-of-line test file, review artifact redacted before tracking with an enforced guard, announce() feedback added to the four filter handlers, the spec's own hygiene literal removed and the self-check added to the Verification Plan; round-2 advisories folded in (hygiene-gate anchor `:79-95`, the read-degrade chain replacing the under-prefixed `mem_ops` citations, `source()` rename, bidi-format filter, `path:line` splitting dropped, v1 fixture mechanism pinned, FTS cross-match test named via `mem_query_terms`, rebuild-blocking sentence, churn-list completion, remaining anchors corrected).
- r4 (2026-11-14): both round-3 blocking findings resolved (see §Round-4 Resolution Map) — the Commit 1 baselines guard made absence-safe for the packaged-crate scenario CUJ 2 requires, and Technical Plan §10 synced to the corrected Commit 18 (four tables, five aria sites, one announce() per filter handler); round-3 advisories folded in (README anchors restored to 155/153, `src/query/reuse.rs` spelled out in the r3 map, the `mem_ops` label corrected to a missing-prefix note, both reuse.rs test modules moved, bidi inputs pinned in the commit-13 test).
- r5 (2026-11-14): the round-4 blocking finding resolved (see §Round-5 Resolution Map) — the tests side of the LOC budget made explicit: commit 7's e2e tests move to new `tests/cli/reuse_allowlist.rs`, commit 1's packaging guard and commit 24's surface tests move to new `tests/cli/packaging.rs`, both registered in `tests/cli/main.rs`, leaving `tests/cli/reuse.rs` (477) and `tests/cli/p8_impact.rs` (476) untouched; the budget table gained five tests-side rows and the "others" row was reworded to the src side; round-4 advisories folded in (both-mods parenthetical unified at ~440, the `mem_ops.rs:137-141` direct gate cited alongside the degrade chain).
- r6 (2026-11-14): the arbitration round recorded (see §Round-6 Resolution Map) — the Commit 9 coverage test entry renamed to the shipped `coverage_raw_client_still_matches_user_defined_symbol` with two-way pin semantics (the planned `coverage_raw_client_not_in_unresolved` targeted an unreachable dead-code disjunct: static `STOPLIST` disjoint from `RAW_CLIENTS`, dynamic guard excludes raw clients; the hunk stays, no code change), the Commit 4/5 file scopes extended to the hunks they ship (`src/db/memory_query.rs` for commit 4; `schema/memory.sql` + `src/db/memory_v2_tests.rs` for commit 5), and B3 mapping the seven further unmapped hunks (`memory_touch_if_rev` SQL + doc, `memory_touch` doc, the two `--touch` machine faces, the dashboard picker probe, the expired-key e2e, and `supersede_cross_key`'s `ORDER BY rev DESC` probe + doc) into Commits 4/7/15 with the CHANGELOG/memory-store wording corrected from two to three touch paths.
