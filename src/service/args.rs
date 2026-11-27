//! MCP argument parsing: typed `ArgReader` bounds checks and per-family
//! conversion from tool JSON args into `CommandRequest`s.

use super::*;

/// The `ParamSpec` declaring argument `name` of tool `tool`. Every Int/Float
/// read goes through its spec so the declared bounds execute at the MCP
/// boundary instead of living only in the JSON Schema renderer; a missing
/// entry is an internal error (an argument wired into `from_tool_args`
/// without a TOOL_SPECS declaration).
fn param_spec(tool: &str, name: &str) -> Result<&'static ParamSpec> {
    TOOL_SPECS
        .iter()
        .find(|t| t.name == tool)
        .and_then(|t| t.params.iter().find(|p| p.name == name))
        .ok_or_else(|| {
            EgError::Internal(format!(
                "argument '{name}' of tool '{tool}' has no ParamSpec declaration"
            ))
        })
}

/// Build a `CommandRequest` from MCP tool arguments (untrusted JSON: every
/// field is type-checked here; anything else is a usage error).
/// Typed reader over untrusted MCP tool arguments.
struct ArgReader<'a>(&'a serde_json::Value);

impl ArgReader<'_> {
    fn s(&self, key: &str) -> Option<String> {
        self.0.get(key).and_then(|v| v.as_str()).map(str::to_string)
    }

    /// Strict String reader for new params: absent/null is `None`, a string is
    /// accepted, and any other JSON type is a usage error instead of a silent
    /// filter drop.
    fn s_opt_spec(&self, tool: &str, name: &str) -> Result<Option<String>> {
        if !matches!(param_spec(tool, name)?.kind, ParamKind::Str) {
            return Err(EgError::Internal(format!(
                "argument '{name}' of tool '{tool}' is not declared as Str"
            )));
        }
        match self.0.get(name) {
            None | Some(serde_json::Value::Null) => Ok(None),
            Some(serde_json::Value::String(s)) => Ok(Some(s.clone())),
            Some(_) => Err(EgError::Usage(format!("{tool}: '{name}' must be a string"))),
        }
    }

    /// Strict Bool reader for new params: absent/null is `false`, a bool is
    /// accepted, and any other JSON type is a usage error.
    fn b_spec(&self, tool: &str, name: &str) -> Result<bool> {
        if !matches!(param_spec(tool, name)?.kind, ParamKind::Bool) {
            return Err(EgError::Internal(format!(
                "argument '{name}' of tool '{tool}' is not declared as Bool"
            )));
        }
        match self.0.get(name) {
            None | Some(serde_json::Value::Null) => Ok(false),
            Some(serde_json::Value::Bool(b)) => Ok(*b),
            Some(_) => Err(EgError::Usage(format!(
                "{tool}: '{name}' must be a boolean"
            ))),
        }
    }

    fn req(&self, key: &str) -> Result<String> {
        self.s(key)
            .ok_or_else(|| EgError::Usage(format!("missing required argument '{key}'")))
    }

    /// Read an Int argument per its TOOL_SPECS declaration (absent or null
    /// falls back to `default`). A non-integer is a usage error (the module's
    /// untrusted-JSON type contract), and a value above the declared max is
    /// clamped to it — the same clamp the design's cap contract assigns to
    /// every declared max ("N > 10,000 clamped to 10,000"; depth "clamp
    /// 1-10"). Values below min pass through untouched: the documented
    /// below-min policies differ per parameter (`limit` rejects < 1 with
    /// exit 2, depth clamps), so the min stays with the consumer that owns
    /// that contract. A param added to TOOL_SPECS cannot ship without its
    /// max executing here.
    fn n_spec(&self, tool: &str, name: &str, default: usize) -> Result<usize> {
        Ok(self.n_opt_spec(tool, name)?.unwrap_or(default))
    }

    /// The Option form for Int arguments that are genuinely optional inputs
    /// (`ttl`, `mem_export` limit): `None` only when the key is absent/null.
    fn n_opt_spec(&self, tool: &str, name: &str) -> Result<Option<usize>> {
        let ParamKind::Int { max, .. } = param_spec(tool, name)?.kind else {
            return Err(EgError::Internal(format!(
                "argument '{name}' of tool '{tool}' is not declared as Int"
            )));
        };
        let Some(v) = self.0.get(name).filter(|v| !v.is_null()) else {
            return Ok(None);
        };
        let mut n = v.as_u64().ok_or_else(|| {
            EgError::Usage(format!(
                "argument '{name}' must be a non-negative integer, got {v}"
            ))
        })? as usize;
        if let Some(max) = max {
            n = n.min(max.max(0) as usize);
        }
        Ok(Some(n))
    }

    /// Read a Float argument per its TOOL_SPECS declaration (absent or null
    /// falls back to `default`). A non-number is a usage error; out-of-range
    /// or non-finite values are rejected, matching the one declared Float's
    /// consumer contract (`build.rs` rejects outside [0, 1]).
    fn f_spec(&self, tool: &str, name: &str, default: f64) -> Result<f64> {
        let ParamKind::Float { min, max } = param_spec(tool, name)?.kind else {
            return Err(EgError::Internal(format!(
                "argument '{name}' of tool '{tool}' is not declared as Float"
            )));
        };
        let Some(v) = self.0.get(name).filter(|v| !v.is_null()) else {
            return Ok(default);
        };
        let x = v
            .as_f64()
            .filter(|x| x.is_finite() && *x >= min && *x <= max)
            .ok_or_else(|| {
                EgError::Usage(format!(
                    "argument '{name}' must be a finite number in [{min}, {max}], got {v}"
                ))
            })?;
        Ok(x)
    }

    /// Read a StrArray argument, executing the declared `maxItems` bound
    /// (MAX_STR_ARRAY_ITEMS, the same constant the JSON Schema renderer
    /// emits) here at the MCP boundary like every other declared bound:
    /// arrays above the cap are truncated to it, the clamp policy `n_opt_spec`
    /// applies to declared Int maxima. Every element is type-checked before
    /// the cap truncates.
    fn str_array(&self, name: &str) -> Result<Vec<String>> {
        let Some(v) = self.0.get(name).filter(|v| !v.is_null()) else {
            return Ok(Vec::new());
        };
        let arr = v.as_array().ok_or_else(|| {
            EgError::Usage(format!("argument '{name}' must be an array of strings"))
        })?;
        let mut out = Vec::with_capacity(arr.len().min(MAX_STR_ARRAY_ITEMS));
        for item in arr {
            let s = item.as_str().ok_or_else(|| {
                EgError::Usage(format!("argument '{name}' elements must be strings"))
            })?;
            out.push(s.to_string());
        }
        out.truncate(MAX_STR_ARRAY_ITEMS);
        Ok(out)
    }
}

/// Usage error for a tool name the `from_tool_args*` family does not route;
/// shared so the message cannot drift between the families.
fn unknown_tool(tool: &str) -> EgError {
    EgError::Usage(format!("unknown tool '{tool}'"))
}

pub fn from_tool_args(tool: &str, args: &serde_json::Value) -> Result<CommandRequest> {
    let a = ArgReader(args);
    if tool.starts_with("mem_") {
        from_mem_tool(tool, &a)
    } else {
        from_graph_tool(tool, &a)
    }
}

fn from_graph_tool(tool: &str, a: &ArgReader<'_>) -> Result<CommandRequest> {
    match tool {
        "build" => Ok(CommandRequest::Build {
            force: a.b_spec(tool, "force")?,
            max_parse_error_rate: a.f_spec(
                tool,
                "max_parse_error_rate",
                crate::query::DEFAULT_MAX_PARSE_ERROR_RATE,
            )?,
        }),
        "usage" => Ok(CommandRequest::Usage {
            quality: a.b_spec(tool, "quality")?,
            global: a.b_spec(tool, "global")?,
        }),
        "callers" | "callees" => {
            let name = a.req("name")?;
            let limit = a.n_spec(tool, "limit", crate::query::DEFAULT_LIMIT)?;
            let depth = a.n_spec(tool, "depth", crate::query::DEFAULT_DEPTH)?;
            Ok(if tool == "callers" {
                CommandRequest::Callers { name, limit, depth }
            } else {
                CommandRequest::Callees { name, limit, depth }
            })
        }
        "context" => Ok(CommandRequest::Context {
            name: a.req("name")?,
            limit: a.n_spec(tool, "limit", crate::query::DEFAULT_CONTEXT_LIMIT)?,
        }),
        "imports" => Ok(CommandRequest::Imports {
            file: a.req("file")?,
            limit: a.n_spec(tool, "limit", crate::query::DEFAULT_LIMIT)?,
        }),
        "path" => Ok(CommandRequest::Path {
            from: a.req("from")?,
            to: a.req("to")?,
            depth: a.n_spec(tool, "depth", crate::query::DEFAULT_PATH_DEPTH)?,
        }),
        "impact" => Ok(CommandRequest::Impact {
            file: a.req("file")?,
            sites: a.b_spec(tool, "sites")?,
            depth: a.n_spec(tool, "depth", crate::query::DEFAULT_DEPTH)?,
            edges: crate::query::parse_edge_kinds(
                &a.s_opt_spec(tool, "edges")?
                    .unwrap_or_else(|| crate::query::DEFAULT_IMPACT_EDGES.to_string()),
            )?,
            limit: a.n_spec(tool, "limit", crate::query::DEFAULT_LIMIT)?,
        }),
        "symbols" => Ok(CommandRequest::Symbols {
            file: a.req("file")?,
            outline: a.b_spec(tool, "outline")?,
            limit: a.n_spec(tool, "limit", crate::query::DEFAULT_LIMIT)?,
        }),
        "reuse" => parse_reuse_tool(tool, a),
        "search" => Ok(CommandRequest::Search {
            terms: a.req("terms")?,
            limit: a.n_spec(tool, "limit", crate::query::DEFAULT_LIMIT)?,
        }),
        "unresolved" => Ok(CommandRequest::Unresolved {
            names: a.b_spec(tool, "names")?,
            path: a.s_opt_spec(tool, "path")?,
            limit: a.n_spec(tool, "limit", crate::query::DEFAULT_LIMIT)?,
        }),
        "stats" => Ok(CommandRequest::Stats {
            path: a.s_opt_spec(tool, "path")?,
        }),
        _ => Err(unknown_tool(tool)),
    }
}

fn parse_reuse_tool(tool: &str, a: &ArgReader<'_>) -> Result<CommandRequest> {
    if let Some(v) = a.0.get("limit").filter(|v| !v.is_null()) {
        let limit = v.as_u64().ok_or_else(|| {
            EgError::Usage(format!(
                "argument 'limit' must be a non-negative integer, got {v}"
            ))
        })?;
        // 0 is the count-only convention (summary totals, no rows).
        if limit > 100 {
            return Err(EgError::Usage("limit must be 0..=100".to_string()));
        }
    }
    Ok(CommandRequest::Reuse {
        paths: a.str_array("paths")?,
        limit: a.n_spec(tool, "limit", crate::query::DEFAULT_REUSE_LIMIT)?,
        deep_callee: a.b_spec(tool, "deep_callee")?,
        min_confidence: a
            .s_opt_spec(tool, "min_confidence")?
            .unwrap_or_else(|| "low".to_string()),
        propose: a.s_opt_spec(tool, "propose")?,
        callees: a.str_array("callees")?,
        explain: a.b_spec(tool, "explain")?,
        show_suppressed: a.b_spec(tool, "show_suppressed")?,
        coverage: a.str_array("coverage")?,
        judge: a.b_spec(tool, "judge")?,
    })
}

fn from_mem_tool(tool: &str, a: &ArgReader<'_>) -> Result<CommandRequest> {
    match tool {
        "mem_write" => Ok(CommandRequest::MemWrite {
            key: a.req("key")?,
            body: a.req("body")?,
            kind: a.req("kind")?,
            tags: a.s_opt_spec(tool, "tags")?,
            refs: a.s_opt_spec(tool, "refs")?,
            scope: a
                .s_opt_spec(tool, "scope")?
                .unwrap_or_else(|| "durable".to_string()),
            ttl: a.n_opt_spec(tool, "ttl")?.map(|v| v as u64),
            supersedes: a.s_opt_spec(tool, "supersedes")?,
            global: a.b_spec(tool, "global")?,
            touch: a.b_spec(tool, "touch")?,
            confirm: a.b_spec(tool, "confirm")?,
        }),
        "mem_touch" => Ok(CommandRequest::MemTouch {
            key: a.req("key")?,
            global: a.b_spec(tool, "global")?,
        }),
        "mem_query" => {
            let local = a.b_spec(tool, "local")?;
            let global = a.b_spec(tool, "global")?;
            super::scope::StoreScope::from_flags(local, global)?;
            let since = a.s_opt_spec(tool, "since")?;
            let until = a.s_opt_spec(tool, "until")?;
            let window = crate::memory::time::parse_time_window(
                since.as_deref(),
                until.as_deref(),
                "mem query",
            )?;
            let terms = a
                .s_opt_spec(tool, "terms")?
                .map(|t| {
                    if t.trim().is_empty() {
                        vec![t]
                    } else {
                        t.split_whitespace().map(str::to_string).collect()
                    }
                })
                .unwrap_or_default();
            Ok(CommandRequest::MemQuery {
                terms,
                all: a.b_spec(tool, "all")?,
                kind: crate::memory::parse_kind_filter(
                    "mem query",
                    a.s_opt_spec(tool, "kind")?.as_deref(),
                )?,
                scope: crate::memory::parse_scope_filter(
                    "mem query",
                    a.s_opt_spec(tool, "scope")?.as_deref(),
                )?,
                limit: a.n_spec(tool, "limit", crate::memory::DEFAULT_MEM_LIMIT)?,
                include_superseded: a.b_spec(tool, "include_superseded")?,
                global,
                reference: match a.s_opt_spec(tool, "ref")? {
                    Some(v) => Some(v),
                    None => a.s("reference"),
                },
                since: window.since,
                until: window.until,
                local,
                explain: a.b_spec(tool, "explain")?,
                legacy: a.b_spec(tool, "legacy")?,
            })
        }
        "mem_get" => Ok(CommandRequest::MemGet {
            key: a.req("key")?,
            global: a.b_spec(tool, "global")?,
        }),
        "mem_delete" => Ok(CommandRequest::MemDelete {
            key: a.req("key")?,
            global: a.b_spec(tool, "global")?,
        }),
        "mem_gc" => Ok(CommandRequest::MemGc {
            global: a.b_spec(tool, "global")?,
        }),
        "mem_export" => {
            let since = a.s_opt_spec(tool, "since")?;
            let until = a.s_opt_spec(tool, "until")?;
            let window = crate::memory::time::parse_time_window(
                since.as_deref(),
                until.as_deref(),
                "mem export",
            )?;
            Ok(CommandRequest::MemExport {
                format: a.s_opt_spec(tool, "format")?,
                limit: a.n_opt_spec(tool, "limit")?,
                global: a.b_spec(tool, "global")?,
                since: window.since,
                until: window.until,
                scan: a.b_spec(tool, "scan")?,
            })
        }
        "mem_import" => Ok(CommandRequest::MemImport {
            path: a.req("path")?,
            format: a.s_opt_spec(tool, "format")?,
            force: a.b_spec(tool, "force")?,
            global: a.b_spec(tool, "global")?,
        }),
        "mem_reindex" => Ok(CommandRequest::MemReindex {
            global: a.b_spec(tool, "global")?,
            embed: a.b_spec(tool, "embed")?,
        }),
        _ => Err(unknown_tool(tool)),
    }
}
