//! MCP tool registry: parameter specs and the `TOOL_SPECS` table mirrored
//! one-to-one with the CLI surface.

pub const MAX_STR_ARRAY_ITEMS: usize = 1000;

/// JSON type of one MCP tool argument.
pub enum ParamKind {
    Str,
    Int {
        min: i64,
        max: Option<i64>,
    },
    Float {
        min: f64,
        max: f64,
    },
    Bool,
    /// Array of strings, at most 1000 entries.
    StrArray,
}

pub struct ParamSpec {
    pub name: &'static str,
    pub kind: ParamKind,
    pub required: bool,
    pub description: &'static str,
}

pub struct ToolSpec {
    pub name: &'static str,
    pub description: &'static str,
    pub params: &'static [ParamSpec],
    /// Default `eg mcp` surface (without `--all-tools`): the core navigation
    /// tools plus mem_write/mem_query (OQ1 owner decision, 2026-11-05). One
    /// source of truth — a spec without this flag stays behind `--all-tools`
    /// and cannot silently drop off the default set.
    pub core: bool,
}

const LIMIT_PARAM: ParamSpec = ParamSpec {
    name: "limit",
    kind: ParamKind::Int {
        min: 1,
        max: Some(crate::query::MAX_LIMIT as i64),
    },
    required: false,
    description: "Maximum results shown (default 50)",
};

const DEPTH_PARAM: ParamSpec = ParamSpec {
    name: "depth",
    kind: ParamKind::Int {
        min: 1,
        max: Some(crate::query::MAX_DEPTH as i64),
    },
    required: false,
    description: "Transitive depth (default 1)",
};

const NAME_PARAM: ParamSpec = ParamSpec {
    name: "name",
    kind: ParamKind::Str,
    required: true,
    description: "Symbol name or dotted qualname",
};

const FILE_PARAM: ParamSpec = ParamSpec {
    name: "file",
    kind: ParamKind::Str,
    required: true,
    description: "Repo-relative path of the indexed file",
};

const KEY_PARAM: ParamSpec = ParamSpec {
    name: "key",
    kind: ParamKind::Str,
    required: true,
    description: "The note's stable identity",
};

macro_rules! p_str {
    ($name:literal, $req:literal, $desc:literal) => {
        ParamSpec {
            name: $name,
            kind: ParamKind::Str,
            required: $req,
            description: $desc,
        }
    };
}

macro_rules! p_bool {
    ($name:literal, $desc:literal) => {
        ParamSpec {
            name: $name,
            kind: ParamKind::Bool,
            required: false,
            description: $desc,
        }
    };
}

macro_rules! p_int {
    ($name:literal, $min:literal, $max:expr, $req:literal, $desc:literal) => {
        ParamSpec {
            name: $name,
            kind: ParamKind::Int {
                min: $min,
                max: $max,
            },
            required: $req,
            description: $desc,
        }
    };
}

macro_rules! p_str_arr {
    ($name:literal, $desc:literal) => {
        ParamSpec {
            name: $name,
            kind: ParamKind::StrArray,
            required: false,
            description: $desc,
        }
    };
}

pub const TOOL_SPECS: &[ToolSpec] = &[
    ToolSpec {
        name: "build",
        description: "Index the repository into .engram/index.db (full atomic replacement)",
        params: &[
            p_bool!("force", "Re-extract everything (disable the fast path) and bypass the parse-error abort"),
            ParamSpec {
                name: "max_parse_error_rate",
                kind: ParamKind::Float { min: 0.0, max: 1.0 },
                required: false,
                description: "Abort above this fraction of parse-error files (default 0.05)",
            },
        ],
        core: true,
    },
    ToolSpec {
        name: "callers",
        description: "Who calls this symbol (symbol name, qualname, or path:line pointer — not a file path or search terms); direct by default, use depth for transitive callers",
        params: &[NAME_PARAM, DEPTH_PARAM, LIMIT_PARAM],
        core: true,
    },
    ToolSpec {
        name: "callees",
        description: "What does this symbol call — transitive with depth",
        params: &[NAME_PARAM, DEPTH_PARAM, LIMIT_PARAM],
        core: true,
    },
    ToolSpec {
        name: "path",
        description: "Shortest call path between two symbols — hop call-site pointers path:line over resolved edges; dynamic dispatch may hide paths",
        params: &[
            p_str!("from", true, "Symbol the path starts from"),
            p_str!("to", true, "Symbol the path must reach"),
            p_int!("depth", 1, Some(10), false, "Maximum hops (default 10)"),
        ],
        core: true,
    },
    ToolSpec {
        name: "context",
        description: "One-call symbol context (symbol name, qualname, or path:line pointer — not a file path): definition + signature, direct callers, and direct callees; use limit to bound per-section rows",
        params: &[
            NAME_PARAM,
            p_int!("limit", 1, Some(50), false, "Callers/callees shown per section (default 10)"),
        ],
        core: true,
    },
    ToolSpec {
        name: "imports",
        description: "What does this file import — outbound resolved imports; external/unresolved omitted",
        params: &[FILE_PARAM, LIMIT_PARAM],
        core: true,
    },
    ToolSpec {
        name: "impact",
        description: "Files that call into <file> — one row per caller file with a site count; an empty calls-only result names import/inherit counts; sites=true lists every call site; edges/depth extend the dependency set",
        params: &[
            FILE_PARAM,
            p_bool!("sites", "List every call site instead of one row per file"),
            DEPTH_PARAM,
            p_str!("edges", false, "Edge kinds: calls,imports,inherits,all (default calls,imports,inherits)"),
            LIMIT_PARAM,
        ],
        core: true,
    },
    ToolSpec {
        name: "symbols",
        description: "Symbols of one indexed file or directory path (not a symbol name), ordered by line; pass outline=true for signatures",
        params: &[
            p_str!("file", true, "Repo-relative path of the indexed file or directory"),
            p_bool!("outline", "Append the one-line signature column"),
            LIMIT_PARAM,
        ],
        core: true,
    },
    ToolSpec {
        name: "reuse",
        description: "Anti-reinvention scan: detect duplicate helpers and suggest canonical reuse; pass paths for focused scans on edited files, limit=0 for count-only, or judge=true to arbitrate gray-band pairs",
        params: &[
            p_str_arr!("paths", "File paths or symbols to inspect (default: entire index)"),
            p_int!("limit", 0, Some(100), false, "Maximum candidates shown; 0 = count-only summary (default 10)"),
            p_bool!("deep_callee", "Extend the callee footprint to second-hop callees (weight 0.5)"),
            p_str!("min_confidence", false, "Minimum confidence: low, provisional, medium, or high (default low)"),
            p_str!("propose", false, "Propose a signature before writing it; paths are not allowed"),
            p_str_arr!("callees", "Proposed callee names used with propose"),
            p_bool!("explain", "Append detector and reason codes to each candidate"),
            p_bool!("show_suppressed", "Include allowlisted candidates and report the suppressed count"),
            p_str_arr!("coverage", "Opt-in coverage detectors: class, unresolved, tests"),
            p_bool!("judge", "Ask a local eg-judge binary to arbitrate gray-zone candidates"),
        ],
        core: true,
    },
    ToolSpec {
        name: "search",
        description: "Symbol-level search: bm25 and caller-ranked concise interface cards",
        params: &[
            p_str!("terms", true, "Space-separated terms; camelCase/snake_case are split and matched"),
            LIMIT_PARAM,
        ],
        core: true,
    },
    ToolSpec {
        name: "usage",
        description: "Telemetry & performance summary: total calls, p50/p95 latency, error rates, token savings, and anti-reinvention effectiveness",
        params: &[
            p_bool!("quality", "Include latency and error breakdown (default false)"),
            p_bool!("global", "Aggregate telemetry globally across all repositories (~/.engram/usage.log) (default false)"),
        ],
        core: true,
    },
    ToolSpec {
        name: "mem_write",
        description: "Persist a project-memory note (agent-authored prose, never source dumps) — identical content under the same key is a no-op; different content opens a new revision",
        params: &[
            p_str!("key", true, "Stable identity for the note (<= 200 chars, no control characters)"),
            p_str!("body", true, "Note body, agent-authored prose (<= 8192 bytes)"),
            p_str!("kind", true, "One of: decision, gotcha, fact, task, procedure"),
            p_str!("tags", false, "Whitespace-separated lowercase tags (<= 16 x 32 chars)"),
            p_str!("refs", false, "Whitespace-separated qualnames / repo paths the note is about"),
            p_str!("scope", false, "durable (default, never expires) or session (expires at now + ttl, default 7 days)"),
            p_int!("ttl", 1, None, false, "Session lifetime in seconds; only valid with scope=session (default 604800 = 7 days)"),
            p_str!("supersedes", false, "Key whose current note this one replaces (cross-key supersession; unknown target is exit 1)"),
            p_bool!("global", "Write to the cross-project global memory store (~/.engram/memory.db) instead of the local repo"),
            p_bool!("touch", "Refresh created_at and re-assert the invocation's TTL (expires_at; explicit ttl wins, else the 7-day session default) when content_hash matches an existing current note"),
            p_bool!("confirm", "Required for reuse-allow/ notes over MCP"),
        ],
        core: true,
    },
    ToolSpec {
        name: "mem_query",
        description: "Rank current project-memory notes for terms (space-separated keywords, mutually exclusive with all=true); use ref/kind/scope/since/until to filter, explain=true for match reasons",
        params: &[
            p_str!("terms", false, "Space-separated terms (mutually exclusive with all)"),
            p_bool!("all", "List current notes by recency instead of ranking"),
            p_str!("ref", false, "Restrict search to notes referencing this path or symbol"),
            p_str!("kind", false, "Restrict to one kind: decision, gotcha, fact, task, procedure"),
            p_str!("scope", false, "Restrict to one scope: durable or session"),
            p_int!("limit", 1, Some(100), false, "Maximum results shown (default 10)"),
            p_bool!("include_superseded", "With all=true, also list superseded revisions (tagged [superseded])"),
            p_bool!("global", "Restrict search exclusively to the global store (default: searches both local and global)"),
            p_str!("since", false, "Lower bound on created_at: YYYY-MM-DD, YYYY-MM-DDTHH:MM:SSZ, or @unix-seconds"),
            p_str!("until", false, "Exclusive upper bound on created_at: YYYY-MM-DD, YYYY-MM-DDTHH:MM:SSZ, or @unix-seconds"),
            p_bool!("local", "Restrict to the project store only (mutually exclusive with global)"),
            p_bool!("explain", "Add one causal why: line per shown ranked note"),
            p_bool!("legacy", "Use legacy ASCII tokens (diagnostic; bypasses the Unicode index)"),
        ],
        core: true,
    },
    ToolSpec {
        name: "mem_touch",
        description: "Refresh anchored_at (last verified) on the active revision without revision churn",
        params: &[
            KEY_PARAM,
            p_bool!("global", "Touch in the cross-project global memory store"),
        ],
        core: true,
    },
    ToolSpec {
        name: "mem_get",
        description: "Show the current project-memory note for one exact key plus its revision count",
        params: &[
            KEY_PARAM,
            p_bool!("global", "Fetch from global memory store"),
        ],
        core: true,
    },
    ToolSpec {
        name: "mem_delete",
        description: "Hard-delete every revision of one project-memory key (its search-index rows go with it)",
        params: &[
            KEY_PARAM,
            p_bool!("global", "Delete from global memory store"),
        ],
        core: false,
    },
    ToolSpec {
        name: "mem_gc",
        description: "Reclaim project-memory disk: purge expired session notes and superseded revisions older than the 30-day retention clock, then report counts",
        params: &[p_bool!("global", "Run GC on global memory store")],
        core: false,
    },
    ToolSpec {
        name: "mem_export",
        description: "Write current project-memory notes to the result text as JSONL (default) or fenced Markdown; the caveat is reported separately so the payload parses cleanly",
        params: &[
            p_str!("format", false, "Output format: jsonl (default) or markdown"),
            p_int!("limit", 1, None, false, "Take the N most recent notes instead of all of them"),
            p_bool!("global", "Export from global memory store"),
            p_str!("since", false, "Lower bound on created_at: YYYY-MM-DD, YYYY-MM-DDTHH:MM:SSZ, or @unix-seconds"),
            p_str!("until", false, "Exclusive upper bound on created_at: YYYY-MM-DD, YYYY-MM-DDTHH:MM:SSZ, or @unix-seconds"),
            p_bool!("scan", "Run an advisory scan for suspected secrets in exported notes"),
        ],
        core: false,
    },
    ToolSpec {
        name: "mem_import",
        description: "Restore project-memory notes from a JSONL or fenced-Markdown file; CLI also accepts '-' for stdin. Conflicting keys are skipped unless force overwrites them. MCP writes are confined to the project store unless global is set",
        params: &[
            p_str!("path", true, "Repo-relative input file over MCP; CLI also accepts '-' for stdin"),
            p_str!("format", false, "Input format: jsonl or markdown (default: detect from path/first line)"),
            p_bool!("force", "Overwrite a key whose local content differs (default: reject and skip)"),
            p_bool!("global", "Import into the global memory store instead of the project store"),
        ],
        core: false,
    },
    ToolSpec {
        name: "mem_reindex",
        description: "Rebuild the derived Unicode memory index; notes are untouched. With embed, computes note embeddings through the eg-judge text model",
        params: &[
            p_bool!("global", "Rebuild the cross-project global memory store"),
            p_bool!("embed", "Compute note embeddings through the eg-judge text model (EG_JUDGE_TEXT_MODEL)"),
        ],
        core: false,
    },
    ToolSpec {
        name: "unresolved",
        description: "Per-file unresolved-callee counts or unresolved edge sites (candidates only)",
        params: &[
            p_bool!("names", "List unresolved edge sites with file:line instead of per-file counts"),
            p_str!("path", false, "Restrict to a repo-relative file or directory prefix"),
            LIMIT_PARAM,
        ],
        core: false,
    },
    ToolSpec {
        name: "stats",
        description: "Index totals: files, symbols, edges, parse errors, and sizes",
        params: &[
            p_str!("path", false, "Restrict counts to a repo-relative file or directory prefix"),
        ],
        core: false,
    },
];
