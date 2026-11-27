//! CLI subcommand definitions for `eg mem`.

use clap::Subcommand;

#[derive(Subcommand)]
pub enum MemAction {
    /// Persist a note (agent-authored prose; identical content under the same
    /// key is a no-op, different content opens a new revision)
    Write {
        /// Stable identity for the note (<= 200 chars, no control characters)
        key: String,
        /// Note body, agent-authored prose (<= 8192 bytes)
        #[arg(long)]
        body: String,
        /// One of: decision, gotcha, fact, task, procedure
        #[arg(long)]
        kind: String,
        /// Whitespace-separated lowercase tags (<= 16 x 32 chars). Accepts
        /// several values or a repeated flag.
        #[arg(long, num_args = 1..)]
        tags: Vec<String>,
        /// Whitespace-separated qualnames / repo paths the note is about
        /// (<= 16 x 256 chars). Accepts several values or a repeated flag.
        #[arg(long, num_args = 1..)]
        refs: Vec<String>,
        /// Note scope: durable (default, never expires) or session (expires
        /// after the TTL)
        #[arg(long, default_value = "durable")]
        scope: String,
        /// Session lifetime in seconds (only with --scope session; default
        /// 604800 = 7 days)
        #[arg(long)]
        ttl: Option<u64>,
        /// Key whose current note this one replaces (cross-key supersession;
        /// unknown target exits 1, same key exits 2)
        #[arg(long)]
        supersedes: Option<String>,
        /// Persist to the cross-project global memory store (~/.engram/memory.db)
        #[arg(long)]
        global: bool,
        /// Refresh created_at and re-assert the invocation's TTL (expires_at;
        /// explicit --ttl wins, else the 7-day session default) when
        /// content_hash matches an existing current note
        #[arg(long)]
        touch: bool,
        /// Confirm an allowlist write (`reuse-allow/...`) or override durable ref hygiene checks
        #[arg(long)]
        confirm: bool,
        /// Optimistic concurrency check: succeed only if the active revision matches
        #[arg(long)]
        expected_rev: Option<u64>,
    },
    /// Rank current notes for the terms (bm25 + ref-token overlap + recency)
    Query {
        /// Space-separated terms (mutually exclusive with --all)
        terms: Vec<String>,
        /// List current notes by recency instead of ranking
        #[arg(long)]
        all: bool,
        /// Restrict search to notes referencing this path or symbol
        #[arg(long = "ref")]
        reference: Option<String>,
        /// Restrict to one kind: decision, gotcha, fact, task, procedure
        #[arg(long)]
        kind: Option<String>,
        /// Restrict to one scope: durable or session
        #[arg(long)]
        scope: Option<String>,
        /// Maximum results shown (default 10, max 100)
        #[arg(long, default_value_t = crate::memory::DEFAULT_MEM_LIMIT)]
        limit: usize,
        /// With --all, also list superseded revisions (tagged [superseded])
        #[arg(long)]
        include_superseded: bool,
        /// Restrict search exclusively to the global store (~/.engram/memory.db)
        #[arg(long)]
        global: bool,
        /// Lower bound on created_at (last asserted): YYYY-MM-DD, RFC 3339, or @unix-seconds
        #[arg(long)]
        since: Option<String>,
        /// Exclusive upper bound on created_at: YYYY-MM-DD, RFC 3339, or @unix-seconds
        #[arg(long)]
        until: Option<String>,
        /// Restrict to the project store only (default searches local and global)
        #[arg(long)]
        local: bool,
        /// Add one causal why: line per shown ranked note
        #[arg(long)]
        explain: bool,
        /// Use legacy ASCII tokens (diagnostic; bypasses the Unicode index)
        #[arg(long)]
        legacy: bool,
    },
    /// Refresh anchored_at (last verified) on the active revision without revision churn
    Touch {
        /// The note's stable identity
        key: String,
        /// Touch in the cross-project global memory store (~/.engram/memory.db)
        #[arg(long)]
        global: bool,
        /// Optimistic concurrency check: succeed only if the active revision matches
        #[arg(long)]
        expected_rev: Option<u64>,
    },
    /// Show the current note for one exact key plus its revision count
    Get {
        /// The note's stable identity
        key: String,
        /// Fetch from the cross-project global memory store (~/.engram/memory.db)
        #[arg(long)]
        global: bool,
        /// Include the latest superseded or retired revision if no active revision exists
        #[arg(long)]
        include_superseded: bool,
    },
    /// Hard-delete every revision of one key (or soft-retire with --retire)
    Delete {
        /// The note's stable identity
        key: String,
        /// Delete from the cross-project global memory store (~/.engram/memory.db)
        #[arg(long)]
        global: bool,
        /// Soft-retire the active revision instead of hard-deleting (recoverable via eg mem restore)
        #[arg(long)]
        retire: bool,
        /// Optimistic concurrency check: succeed only if the active revision matches
        #[arg(long)]
        expected_rev: Option<u64>,
    },
    /// Restore a soft-retired note back into the active memory index
    Restore {
        /// The note's stable identity
        key: String,
        /// Restore in the cross-project global memory store (~/.engram/memory.db)
        #[arg(long)]
        global: bool,
    },
    /// Classify broken and modified memory anchors with git and index evidence
    Triage {
        /// Restrict triage to one exact memory key
        #[arg(long)]
        key: Option<String>,
        /// Cursor pagination: list items strictly after this key
        #[arg(long)]
        after_key: Option<String>,
        /// Maximum items shown (default 10, max 25)
        #[arg(long, default_value_t = crate::memory::triage::TRIAGE_DEFAULT_LIMIT)]
        limit: usize,
        /// Disable read-only git rename/split enrichment
        #[arg(long)]
        no_git: bool,
        /// Triage the cross-project global memory store (~/.engram/memory.db)
        #[arg(long)]
        global: bool,
        /// Emit structured JSON Evidence Packet
        #[arg(long)]
        json: bool,
    },
    /// Write current notes to stdout as JSONL (default) or fenced Markdown
    /// (Git-versionable); the caveat goes to stderr so pipelines parse stdout
    Export {
        /// Output format: jsonl (default, restore format) or markdown
        #[arg(long)]
        format: Option<String>,
        /// Take the N most recent notes instead of all of them
        #[arg(long)]
        limit: Option<usize>,
        /// Export from the cross-project global memory store (~/.engram/memory.db)
        #[arg(long)]
        global: bool,
        /// Lower bound on created_at: YYYY-MM-DD, RFC 3339, or @unix-seconds
        #[arg(long)]
        since: Option<String>,
        /// Exclusive upper bound on created_at: YYYY-MM-DD, RFC 3339, or @unix-seconds
        #[arg(long)]
        until: Option<String>,
        /// Report suspected secrets in exported notes on stderr (advisory; exit stays 0)
        #[arg(long)]
        scan: bool,
    },
    /// Restore notes from a JSONL or fenced-Markdown export (use '-' for stdin).
    /// Conflicting keys are skipped unless --force overwrites them.
    Import {
        /// Input file path, or '-' to read stdin
        path: String,
        /// Input format: jsonl or markdown (default: detect from path/first line)
        #[arg(long)]
        format: Option<String>,
        /// Overwrite a key whose local content differs (default: reject and skip)
        #[arg(long, alias = "overwrite")]
        force: bool,
        /// Import into the cross-project global memory store (~/.engram/memory.db)
        #[arg(long)]
        global: bool,
    },
    /// Reclaim disk: purge expired session notes and superseded revisions
    /// older than the 30-day retention clock, then report counts
    Gc {
        /// Run GC on the cross-project global memory store (~/.engram/memory.db)
        #[arg(long)]
        global: bool,
    },
    /// Rebuild the derived Unicode memory index (derived data only; notes are untouched)
    Reindex {
        /// Rebuild the cross-project global memory store (~/.engram/memory.db)
        #[arg(long)]
        global: bool,
        /// Compute note embeddings through the eg-judge text model
        /// ($EG_JUDGE_TEXT_MODEL) for every current note; notes stay NULL
        /// when the model is not configured
        #[arg(long)]
        embed: bool,
    },
}
