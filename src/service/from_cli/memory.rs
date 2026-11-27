//! CLI-to-request conversion for the memory family. Kept in a submodule so
//! the parent mapper stays under the 500-line hygiene cap.

use crate::cli::MemAction;
use crate::error::Result;
use crate::journal::JournalArgs;
use crate::service::scope::StoreScope;
use crate::service::{mem_query_args, mem_write_args, CommandRequest};

impl CommandRequest {
    /// Memory subcommands. `write`/`query`/`export` destructure through their
    /// own request helpers (the same surface-boundary split `impact_request`
    /// uses).
    pub(super) fn from_cli_memory(cmd: crate::cli::Cmd) -> Result<CommandRequest> {
        let crate::cli::Cmd::Mem { action } = cmd else {
            unreachable!("from_cli_memory called with a non-matching command")
        };
        match action {
            MemAction::Write { .. } => Self::mem_write_request(action),
            MemAction::Touch {
                key,
                global,
                expected_rev,
            } => Ok(CommandRequest::MemTouch {
                key,
                global,
                expected_rev: Self::parse_expected_rev(expected_rev)?,
            }),
            MemAction::Query { .. } => Self::mem_query_request(action),
            MemAction::Get {
                key,
                global,
                include_superseded,
            } => Ok(CommandRequest::MemGet {
                key,
                global,
                include_superseded,
            }),
            MemAction::Delete {
                key,
                global,
                retire,
                expected_rev,
            } => {
                let expected_rev = Self::parse_expected_rev(expected_rev)?;
                if retire {
                    Ok(CommandRequest::MemRetire {
                        key,
                        global,
                        expected_rev,
                    })
                } else {
                    Ok(CommandRequest::MemDelete {
                        key,
                        global,
                        expected_rev,
                    })
                }
            }
            MemAction::Restore { key, global } => Ok(CommandRequest::MemRestore { key, global }),
            MemAction::Triage {
                key,
                after_key,
                limit,
                no_git,
                global,
                json,
            } => Ok(CommandRequest::MemTriage {
                key,
                after_key,
                limit,
                no_git,
                global,
                json,
            }),
            MemAction::Export { .. } => Self::mem_export_request(action),
            MemAction::Import {
                path,
                format,
                force,
                global,
            } => Ok(CommandRequest::MemImport {
                path,
                format,
                force,
                global,
            }),
            MemAction::Gc { global } => Ok(CommandRequest::MemGc { global }),
            MemAction::Reindex { global, embed } => {
                Ok(CommandRequest::MemReindex { global, embed })
            }
        }
    }

    /// The `mem write` request shape, destructured from the CLI variant.
    fn mem_write_request(action: MemAction) -> Result<CommandRequest> {
        let MemAction::Write {
            key,
            body,
            kind,
            tags,
            refs,
            scope,
            ttl,
            supersedes,
            global,
            touch,
            confirm,
            expected_rev,
        } = action
        else {
            unreachable!("mem_write_request called with a non-write action")
        };
        Ok(CommandRequest::MemWrite {
            key,
            body,
            kind,
            tags: Self::join_list(tags),
            refs: Self::join_list(refs),
            scope,
            ttl,
            supersedes,
            global,
            touch,
            confirm,
            expected_rev: Self::parse_expected_rev(expected_rev)?,
        })
    }

    fn parse_expected_rev(raw: Option<u64>) -> Result<Option<i64>> {
        match raw {
            None => Ok(None),
            Some(0) => Err(crate::error::EgError::Usage(
                "expected_rev must be >= 1".to_string(),
            )),
            Some(v) => i64::try_from(v)
                .map(Some)
                .map_err(|_| crate::error::EgError::Usage("expected_rev out of range".to_string())),
        }
    }

    /// Collapse `--tags a b` and `--tags a --tags b` into the one
    /// space-separated string the request carries.
    fn join_list(values: Vec<String>) -> Option<String> {
        let joined = values.join(" ");
        (!joined.is_empty()).then_some(joined)
    }

    /// The `mem query` request shape, destructured from the CLI variant.
    fn mem_query_request(action: MemAction) -> Result<CommandRequest> {
        let MemAction::Query {
            terms,
            all,
            reference,
            kind,
            scope,
            limit,
            include_superseded,
            global,
            since,
            until,
            local,
            explain,
            legacy,
        } = action
        else {
            unreachable!("mem_query_request called with a non-query action")
        };
        StoreScope::from_flags(local, global)?;
        let window = crate::memory::time::parse_time_window(
            since.as_deref(),
            until.as_deref(),
            "mem query",
        )?;
        Ok(CommandRequest::MemQuery {
            terms,
            all,
            kind: crate::memory::parse_kind_filter("mem query", kind.as_deref())?,
            scope: crate::memory::parse_scope_filter("mem query", scope.as_deref())?,
            limit,
            include_superseded,
            global,
            reference,
            since: window.since,
            until: window.until,
            local,
            explain,
            legacy,
        })
    }

    /// The `mem export` request shape, destructured from the CLI variant.
    fn mem_export_request(action: MemAction) -> Result<CommandRequest> {
        let MemAction::Export {
            format,
            limit,
            global,
            since,
            until,
            scan,
        } = action
        else {
            unreachable!("mem_export_request called with a non-export action")
        };
        let window = crate::memory::time::parse_time_window(
            since.as_deref(),
            until.as_deref(),
            "mem export",
        )?;
        Ok(CommandRequest::MemExport {
            format,
            limit,
            global,
            since: window.since,
            until: window.until,
            scan,
        })
    }

    pub(super) fn mem_journal_args(&self) -> JournalArgs {
        match self {
            CommandRequest::MemWrite { .. } => mem_write_args(self),
            CommandRequest::MemQuery { .. } => mem_query_args(self),
            CommandRequest::MemTouch {
                key,
                global,
                expected_rev,
            }
            | CommandRequest::MemDelete {
                key,
                global,
                expected_rev,
            }
            | CommandRequest::MemRetire {
                key,
                global,
                expected_rev,
            } => vec![
                ("key", key.clone()),
                ("global", global.to_string()),
                (
                    "expected_rev",
                    expected_rev.map_or_else(String::new, |r| r.to_string()),
                ),
            ],
            CommandRequest::MemGet {
                key,
                global,
                include_superseded,
            } => vec![
                ("key", key.clone()),
                ("global", global.to_string()),
                ("include_superseded", include_superseded.to_string()),
            ],
            CommandRequest::MemRestore { key, global } => {
                vec![("key", key.clone()), ("global", global.to_string())]
            }
            CommandRequest::MemTriage {
                key,
                after_key,
                limit,
                no_git,
                global,
                json,
            } => vec![
                ("key", key.clone().unwrap_or_default()),
                ("after_key", after_key.clone().unwrap_or_default()),
                ("limit", limit.to_string()),
                ("no_git", no_git.to_string()),
                ("global", global.to_string()),
                ("json", json.to_string()),
            ],
            CommandRequest::MemGc { global } => vec![("global", global.to_string())],
            CommandRequest::MemReindex { global, embed } => {
                vec![("global", global.to_string()), ("embed", embed.to_string())]
            }
            CommandRequest::MemExport {
                format,
                limit,
                global,
                since,
                until,
                scan,
            } => vec![
                ("format", format.clone().unwrap_or_default()),
                ("limit", limit.map_or_else(String::new, |n| n.to_string())),
                ("global", global.to_string()),
                ("since", since.map_or_else(String::new, |n| n.to_string())),
                ("until", until.map_or_else(String::new, |n| n.to_string())),
                ("scan", scan.to_string()),
            ],
            CommandRequest::MemImport {
                format,
                force,
                global,
                ..
            } => vec![
                ("format", format.clone().unwrap_or_default()),
                ("force", force.to_string()),
                ("global", global.to_string()),
            ],
            _ => unreachable!("mem_journal_args called with a non-memory request"),
        }
    }
}
