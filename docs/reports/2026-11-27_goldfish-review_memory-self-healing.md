# Goldfish Review Report

- Target: `docs/designs/2026-11-27_memory-self-healing.md` (`FeatureRange`: L1–L253 / `DetailedRange`: L254–L451)
- Date: 2026-11-27
- Lenses run: 5 (Native parallel isolated subagents: `Zoom-Out`, `Comprehension`, `Design Critic`, `Implementation Readiness`, `Codebase Details`)

## Findings

### F1: No Recovery Path for Soft-Retired Notes (Critical)
- Severity: Critical
- Lens: Design Critic, Comprehension
- Scope: `docs/designs/2026-11-27_memory-self-healing.md#L105`, `#L184-L196`
- Description: CUJ 4 claims soft-retired notes (`superseded_by = -1`) are recoverable for 30 days, yet `mem_get` and `mem_export` hide retired rows and no restore/inspect command is specified.
- Recommendation: Add CLI `eg mem restore <key> [--global]` (re-activates the latest `superseded_by = -1` row as `rev + 1` inside `BEGIN IMMEDIATE` and re-indexes `memories_fts` + `memories_fts_v2`) and `eg mem get --include-superseded <key>`.

### F2: `opportunistic_gc` Runs Before `--expected-rev` Check (Critical)
- Severity: Critical
- Lens: Design Critic
- Scope: `docs/designs/2026-11-27_memory-self-healing.md#L97`, `#L192`, `src/db/memory_write.rs#L135`
- Description: `opportunistic_gc` currently executes in its own transaction before `try_write_revision` validates `expected_rev`. A conflicting write (`EgError::Conflict`) would still purge >50 rows, violating CUJ 3's zero-mutation invariant.
- Recommendation: Move `opportunistic_gc` strictly after `try_write_revision` commits `MemWriteOutcome::Written` and swallow GC errors so committed writes never fail.

### F3: M2 Git Lineage Skips #1 Broken Category (`doc_or_report`, 42.1%) & Unverified `>= 95%` Target (Critical)
- Severity: Critical
- Lens: Zoom-Out, Design Critic
- Scope: `docs/designs/2026-11-27_memory-self-healing.md#L23-L27`, `#L33-L36`, `#L162`
- Description: `doc_or_report` accounts for `16/38` (`42.1%`) of baseline broken refs, yet M2 probes git only for `MissingCodePath`. Additionally, the `>= 95%` currency target has no matching Goal or exam.
- Recommendation: Probe `MissingDocOrReport` alongside `MissingCodePath` in M2's bounded git pass, and qualify `>= 95%` as a post-Batch-Repair exam verified by `eg audit` `memory.refs`.

### F4: Misattributed Symbols & 13 Omitted Files in File Ledger (Critical)
- Severity: Critical
- Lens: Codebase Details, Implementation Readiness
- Scope: `docs/designs/2026-11-27_memory-self-healing.md#L316-L418`
- Description:
  1. `render_mem_hit` and `mem_row_tag` (`" [superseded]"`) live in `src/service/mem_query.rs` (omitted), not `src/service/mem_ops.rs` or `src/memory/rank.rs`. `render_mem_hit` must place `refs:` before `revs:` (omitted when empty) to keep `tests/cli/memory_basics.rs:70,191` green.
  2. `ResolvedRef` and `mod refs;` live in `src/db/query_state.rs` (omitted), not only `src/db/query_state/refs.rs`.
  3. 11 additional required files are omitted: `src/db.rs`, `src/service/request.rs`, `src/service/from_cli.rs`, `src/service/run.rs`, `src/service/mem_import.rs` (`MemNoteInput` literal), `src/mcp.rs`, `src/dashboard/server.rs`, `src/dashboard/assets/core.js` (`mem_retire` SSE trigger), `src/dashboard/data/summary.rs` (`classify()`), `src/audit/collect/parse.rs`, `src/audit/checks/effectiveness.rs`, `src/audit/render.rs`, `tests/mcp/parity.rs`, `scripts/install.sh`, unconditional `src/cli/mem.rs`, and the 5 test modules constructing `MemNoteInput`.
- Recommendation: Correct symbol ownership and add every omitted file to the File Ledger.

### F5: `ArgReader` Strict Unknown-Key Rejection Breaks MCP `"root"` and `mem_query` `"reference"` (Critical)
- Severity: Critical
- Lens: Codebase Details
- Scope: `docs/designs/2026-11-27_memory-self-healing.md#L397`, `src/mcp.rs#L67-L72`, `src/service/args.rs#L324-L327`
- Description: `src/mcp.rs` injects `"root"` outside `ToolSpec.params` on every tool, and `src/service/args.rs` accepts `"reference"` as a fallback alias on `mem_query`. Rejecting unknown keys strictly against `TOOL_SPECS.params` breaks those callers.
- Recommendation: Allowlist `"root"` globally and `"reference"` on `mem_query` in `ArgReader`'s unknown-key validation.

### F6: `memory_touch_if_rev` Return Type & `NotFound` vs `Conflict` Precedence Ambiguity (Critical)
- Severity: Critical
- Lens: Codebase Details, Implementation Readiness, Design Critic, Comprehension
- Scope: `docs/designs/2026-11-27_memory-self-healing.md#L185-L202`, `#L355-L383`, `src/db/memory_write.rs#L315-L336`
- Description: `memory_touch_if_rev` returns `Result<Option<i64>>`, collapsing missing key, inactive row, and rev mismatch into `None`; `memory_retire` step 1 returned `NotFound` before checking `expected_rev`; `expected_rev` negative-value rejection and MCP `confirm == true` validation were underspecified.
- Recommendation: Introduce a shared `CasOutcome { Applied(i64), NotFound, Conflict { actual: Option<i64> } }` for `memory_retire`, `memory_delete_if_rev`, and `memory_touch_if_rev`; return `Conflict` whenever `expected_rev` is supplied and does not match an active revision; type CLI `--expected-rev` as `Option<u64>` (`min: 0`, `min: 1` on `mem_retire`) and validate `confirm == true` explicitly.

### F7: `judge_proc` Reuse Breaks `PATH` and Collapses `GitStats` Timeouts vs Failures (Critical)
- Severity: Critical
- Lens: Codebase Details, Implementation Readiness, Design Critic
- Scope: `docs/designs/2026-11-27_memory-self-healing.md#L165-L170`, `#L350-L353`, `src/memory/judge_proc.rs#L74-L206`
- Description: `judge_proc::base_command` calls `env_clear()` (dropping `PATH`, breaking `Command::new("git")`) and `run_bounded` returns `Option<String>` (cannot distinguish timeouts from failures). Also missing: `GIT_CONFIG_GLOBAL=/dev/null`, `--no-show-signature`, `-z` NUL parsing, filename sanitization, exact `git log` invocation, and running `git status` before per-ref probes.
- Recommendation: Implement a self-contained bounded git runner in `src/memory/triage/git.rs` returning `GitRunOutcome { Ok(Vec<u8>), TimedOut, Failed }`, preserving `PATH`, stripping `GIT_*`, setting `GIT_CONFIG_GLOBAL=/dev/null`, passing `--no-show-signature -z`, sanitizing both subjects and filenames via `crate::query::sanitize`, and running `git status` once up front.

### F8: Phase 2 Non-Buildable Due to Phase 3 Placement of `UNPRICED_TOOLS` & `EgError` Split (Critical)
- Severity: Critical
- Lens: Implementation Readiness
- Scope: `docs/designs/2026-11-27_memory-self-healing.md#L312-L314`, `src/service.rs#L343-L359`
- Description: Phase 2 adds `CommandRequest::MemTriage` while deferring `UNPRICED_TOOLS` and `mem_journal_args` to Phase 3, breaking `service.rs` exhaustiveness tests in Phase 2.
- Recommendation: Update `UNPRICED_TOOLS`, `mem_journal_args`, and `dashboard/data/summary.rs` for `MemTriage` in Phase 2, and introduce `EgError::Hygiene` in Phase 1 vs `EgError::Conflict` in Phase 3.

### F9: Zero-Decision Gaps in M1 & M2 Schemas, Precedence, Pagination, and Path-vs-Symbol Logic (Critical)
- Severity: Critical
- Lens: Implementation Readiness, Comprehension, Design Critic, Zoom-Out
- Scope: `docs/designs/2026-11-27_memory-self-healing.md#L135-L180`, `#L268-L353`
- Description:
  1. Missing complete Rust enum variants (`RefFailureKind`, `TriageReason`, `TriageAction`, `IndexState`, `CommitContext`, `SymbolCandidate`, `HygieneOutcome`), ordered `classify_note_pure` precedence table, `TriageReason -> (triage_label, suggested_cli)` table, `omitted_count` in JSON schema, `--after-key` pagination flag, whole-item `16,384`-byte truncation rule, and `render_triage_text` format.
  2. M1 `has_known_file_ext` negative matching misclassifies extensionless/unlisted files (`Dockerfile`, `.env`, `debug.log`) as symbols; relative symlink escapes were unchecked; `validate_and_normalize_refs` ordering relative to `normalize_note` and `GitignoreBuilder` nested file loading + non-existent `is_dir` handling were unspecified; `ENGRAM_MEM_HYGIENE=0` behavior conflicted between line 135 and line 428.
  3. `mem_triage` `index.db` symbol queries lacked a per-call cap (`<= 20` refs on returned `limit` notes only).
- Recommendation: Freeze every schema, precedence table, pagination cursor (`--after-key`), path-vs-symbol rule (bypass only on `::` or indexed symbol in `index.db`), relative symlink check, `normalize_note` ordering, and bounded symbol lookup in `DetailedRange`.

### F10: MCP `core: true` Exposure for `mem_triage` & `mem_retire` vs `AGENTS.md` §5 Pillar 3 (Important — Escalated)
- Severity: Important
- Lens: Zoom-Out
- Scope: `docs/designs/2026-11-27_memory-self-healing.md#L174`, `#L196`, `#L397`
- Description: `AGENTS.md` §5 Pillar 3 keeps maintenance/audit commands out of mandatory agent routing, and `src/service/tools.rs` restricts `core: true` to core navigation + `mem_write`/`mem_query`/`mem_get`/`mem_touch`. Setting `core: true` on `mem_triage` and `mem_retire` expands every default MCP session schema for batch maintenance.
- Recommendation: Decide whether `mem_triage` and `mem_retire` should be `core: false` (available via `--all-tools` and CLI, using `mem_get` + `mem_write` for default JIT) or remain `core: true`.

### F11: Bundled Observability, Kill-Switch & Optimization Scope Creep (Important — Escalated / Trimmed)
- Severity: Important
- Lens: Zoom-Out, Design Critic, Codebase Details
- Scope: `docs/designs/2026-11-27_memory-self-healing.md#L135`, `#L149`, `#L241-L246`, `#L365-L368`, `#L400-L408`
- Description: Bundles unrequested changes without measured bottlenecks or Goals: `PRAGMA cache_size = -2048`, `src/memory/secret.rs` URL check, `src/journal/meta.rs` + `src/mcp.rs` `request_id` plumbing (threatening `src/journal.rs`'s 497/500 LOC ceiling), 3 env kill switches, and MCP `json: bool` toggle on `mem_triage`.
- Recommendation: Drop `cache_size` and `secret.rs` from this spec; decide whether to keep full `request_id` + `audit` telemetry + kill switches in Phase 3 or trim to minimal `EgError` classes + `audit/collect.rs` mutating `exit 2` counting.

### F12: `FeatureRange` Altitude Drop, Missing M1/M2 Alternatives & `Non-Goals` Contradiction (Important)
- Severity: Important
- Lens: Zoom-Out, Design Critic, Comprehension
- Scope: `docs/designs/2026-11-27_memory-self-healing.md#L58`, `#L133-L228`
- Description: `FeatureRange` leaks Rust function signatures, literal SQL, crate APIs, and file paths; `Alternatives Considered` lacks >=2 alternatives each for M1 and M2; `Non-Goals` line 58 contradicts the M1 `--global` absolute-path guard at line 148; `Rollout & Rollback` omits that post-retire `mem_export` excludes retired rows and pre-M3 binaries display `-1` as `[superseded]`.
- Recommendation: Lift `FeatureRange` back to system contracts/invariants, move all code/SQL/flags into `DetailedRange`, add M1/M2 alternatives, and align `Non-Goals` + `Rollout & Rollback`.

### F13: Remaining Codebase Ground-Truth & Test Plan Fixes (Important)
- Severity: Important
- Lens: Codebase Details, Implementation Readiness
- Scope: `docs/designs/2026-11-27_memory-self-healing.md#L260-L266`, `#L372`, `#L405-L451`
- Description:
  1. `EgError::Conflict` / `Hygiene` `Display` must use standard `eg: <class>: <detail>` prefix.
  2. `normalized_count` cannot be logged in `mem_write_args` (runs before `run_mem_write`); must record post-execution via `journal::record_result`.
  3. Dashboard `memories.rs` must exempt `IndexOpenError::Missing` from `degraded`, emit `null` triage fields for `!is_current`, and bind copy button via `core.js::initDelegation` (no inline `onclick=` per `client_contract_tests`).
  4. `budget(tool)` requires `(warn_ms, critical_ms)` tuple (e.g. `(1500.0, 3000.0)` for `mem_triage`), `MemoryFacts::notes_truncated` must be surfaced in `audit/checks/effectiveness.rs` + `audit/render.rs`, `src/dashboard/server/http.rs` is 307 LOC (not ~200), Test Plan needs exact `#[test]` names + target files, and Verification Plan needs `AGENTS.md` §7 doc-verification checks.
- Recommendation: Apply all ground-truth corrections directly in `DetailedRange`.

## Reconciliation

| Finding | Decision | Evidence |
| :--- | :--- | :--- |
| F1 (Recovery path for retired notes) | `PROPOSE FIX` | `docs/designs/2026-11-27_memory-self-healing.md#L105`, `#L184-L196` |
| F2 (`opportunistic_gc` before CAS check) | `PROPOSE FIX` | `src/db/memory_write.rs#L135` |
| F3 (Probe `doc_or_report` in M2 + exam) | `PROPOSE FIX` | Baseline `16/38` (`42.1%`) `doc_or_report` in `#L35` |
| F4 (Misattributed symbols & 13 missing files) | `PROPOSE FIX` | `src/service/mem_query.rs#L159-L166,L444-L455`, `src/db/query_state.rs#L321-L353`, `tests/mcp/parity.rs#L37-L64` |
| F5 (`ArgReader` allowlist `"root"`/`"reference"`) | `PROPOSE FIX` | `src/mcp.rs#L67-L72`, `src/service/args.rs#L324-L327` |
| F6 (`CasOutcome`, `expected_rev: Option<u64>`, `confirm==true`) | `PROPOSE FIX` | `src/db/memory_write.rs#L315-L336`, `src/mcp.rs#L430` |
| F7 (Dedicated bounded git runner + `PATH` + `-z`) | `PROPOSE FIX` | `src/memory/judge_proc.rs#L74-L206` |
| F8 (Phase 2 `UNPRICED_TOOLS` & `EgError` split) | `PROPOSE FIX` | `src/service.rs#L343-L359` |
| F9 (Zero-Decision schemas, precedence, path-vs-symbol, pagination) | `PROPOSE FIX` | `docs/designs/2026-11-27_memory-self-healing.md#L135-L180,#L268-L353` |
| F10 (`mem_triage`/`mem_retire` `core: true` vs `core: false`) | `PROPOSE FIX` | Owner decision: `core: false` for `mem_triage`, `mem_retire`, `mem_restore` (`AGENTS.md` §5 Pillar 3) |
| F11 (Observability/kill-switches scope vs minimal spec) | `PROPOSE FIX` | Owner decision: lean scope (dropped `cache_size`, `secret.rs`, `src/journal/meta.rs`/`request_id`, MCP `json` param; kept `audit` mutating exit-2 + `notes_truncated` + 2 kill switches) |
| F12 (`FeatureRange` altitude, M1/M2 alternatives, `Non-Goals`) | `PROPOSE FIX` | `docs/designs/2026-11-27_memory-self-healing.md#L58,#L133-L228` |
| F13 (Codebase ground-truth & Test/Verification details) | `PROPOSE FIX` | `src/error.rs#L1-L2`, `src/service.rs#L72-L81`, `src/db/open.rs#L57-L62`, `src/dashboard.rs#L179-L184` |
