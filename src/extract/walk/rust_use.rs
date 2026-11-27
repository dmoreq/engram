//! Rust `use`-tree parsing: structural resolution of use declarations, lists,
//! grouped aliases, wildcard imports, and path prefixing.

use super::*;

#[derive(Clone, Copy)]
pub(super) struct UseTreeCtx<'a> {
    src: &'a [u8],
    prefix: &'a [String],
    scope: &'a str,
    alias_scope: &'a str,
    crate_root: Option<&'a str>,
    lib_root: Option<&'a str>,
    crate_name: Option<&'a str>,
    line: usize,
    maybe_external: bool,
}

/// Rust use declarations: resolve the structural use tree (`use_list`,
/// `scoped_use_list`, `use_as_clause`, `use_wildcard`) to module/symbol
/// candidates. `crate::` maps to the file's top-level directory, `self::` to
/// the current module scope, and `super::` to its parent. Plain paths are
/// qualified against `scope` and validated against `module_paths` in Phase B.
pub(super) fn record_use_decl(
    node: Node<'_>,
    src: &[u8],
    fx: &mut FileExtraction,
    scope: &str,
    alias_scope: &str,
) {
    let Some(arg) = node.child_by_field_name("argument") else {
        return;
    };
    let crate_root = fx.crate_root.clone();
    let lib_root = fx.lib_root.clone();
    let crate_name = fx.crate_name.clone();
    // In a binary crate root (`src/main.rs` -> `fx.module == "src.main"`),
    // top-level relative `use sub::item;` resolves from `crate_root` (`"src"`).
    let effective_scope = if scope == fx.module
        && (fx.module == "main" || fx.module.ends_with(".main"))
        && crate_root.is_some()
    {
        crate_root.as_deref().unwrap_or(scope)
    } else {
        scope
    };
    let ctx = UseTreeCtx {
        src,
        prefix: &[],
        scope: effective_scope,
        alias_scope,
        crate_root: crate_root.as_deref(),
        lib_root: lib_root.as_deref(),
        crate_name: crate_name.as_deref(),
        line: node.start_position().row + 1,
        maybe_external: false,
    };
    walk_use_tree(arg, ctx, fx);
}

pub(super) fn walk_use_tree(node: Node<'_>, ctx: UseTreeCtx<'_>, fx: &mut FileExtraction) {
    match node.kind() {
        "use_list" => {
            let mut c = node.walk();
            for child in node.named_children(&mut c) {
                walk_use_tree(child, ctx, fx);
            }
        }
        "scoped_use_list" => emit_scoped_use_list(node, ctx, fx),
        "use_as_clause" => {
            let Some(path) = node.child_by_field_name("path") else {
                return;
            };
            let alias = node.child_by_field_name("alias").map(|n| text(n, ctx.src));
            emit_use_leaf(path, ctx, alias, fx);
        }
        "use_wildcard" => emit_use_wildcard(node, ctx, fx),
        _ => emit_use_leaf(node, ctx, None, fx),
    }
}

fn emit_scoped_use_list(node: Node<'_>, ctx: UseTreeCtx<'_>, fx: &mut FileExtraction) {
    let Some(path) = node.child_by_field_name("path") else {
        return;
    };
    let Some(list) = node.child_by_field_name("list") else {
        return;
    };
    let Some((base, maybe_external)) = resolve_use_path(path, ctx) else {
        return;
    };
    if !base.is_empty() {
        fx.import_edges.push(
            ImportEdgeCandidate::module(ctx.line, base.join("."))
                .with_maybe_external(maybe_external),
        );
    }
    walk_use_tree(
        list,
        UseTreeCtx {
            prefix: &base,
            maybe_external,
            ..ctx
        },
        fx,
    );
}

fn emit_use_wildcard(node: Node<'_>, ctx: UseTreeCtx<'_>, fx: &mut FileExtraction) {
    // tree-sitter-rust has no `path` field here: `crate::a::*` puts the
    // path in a named child, while a grouped `{ * }` has none.
    let mut c = node.walk();
    let path = node
        .child_by_field_name("path")
        .or_else(|| node.named_children(&mut c).next());
    let (full, maybe_external) = match path {
        Some(path) => match resolve_use_path(path, ctx) {
            Some(res) => res,
            None => return,
        },
        None => (ctx.prefix.to_vec(), ctx.maybe_external),
    };
    if full.is_empty() {
        return;
    }
    let qn = full.join(".");
    fx.bindings.stars.push((qn.clone(), ctx.line));
    if !ctx.alias_scope.is_empty() {
        fx.bindings
            .star_scopes
            .insert((qn.clone(), ctx.line), ctx.alias_scope.to_string());
    }
    if maybe_external {
        fx.bindings
            .maybe_external_stars
            .insert((qn.clone(), ctx.line));
    }
    fx.import_edges
        .push(ImportEdgeCandidate::module_root(ctx.line, qn).with_maybe_external(maybe_external));
}

pub(super) fn emit_use_leaf(
    node: Node<'_>,
    ctx: UseTreeCtx<'_>,
    alias: Option<String>,
    fx: &mut FileExtraction,
) {
    let Some((full, maybe_external)) = resolve_use_path(node, ctx) else {
        return;
    };
    if full.is_empty() {
        return;
    }
    if emit_module_leaf(node, ctx, &full, &alias, maybe_external, fx) {
        return;
    }
    let Some((name, parent)) = full.split_last() else {
        return;
    };
    let module = parent.join(".");
    let bound = alias.unwrap_or_else(|| name.clone());
    if maybe_external {
        fx.bindings
            .maybe_external_aliases
            .insert((ctx.alias_scope.to_string(), bound.clone()));
    }
    fx.import_edges.push(
        ImportEdgeCandidate::module(ctx.line, module.clone()).with_maybe_external(maybe_external),
    );
    fx.import_edges.push(
        ImportEdgeCandidate::symbol(ctx.line, module.clone(), name.clone())
            .with_maybe_external(maybe_external),
    );
    fx.bindings.aliases.push((
        ctx.alias_scope.to_string(),
        bound,
        ImportTarget::Symbol {
            module,
            name: name.clone(),
        },
    ));
}

/// A module-root leaf (`use a::b` where `b` is a module, or `use self`) emits a
/// module edge plus an optional binding. Returns false for a symbol leaf.
fn emit_module_leaf(
    node: Node<'_>,
    ctx: UseTreeCtx<'_>,
    full: &[String],
    alias: &Option<String>,
    maybe_external: bool,
    fx: &mut FileExtraction,
) -> bool {
    let qn = full.join(".");
    let raw = text(node, ctx.src);
    let marker_only = raw == "self";
    let after_marker = raw.split("::").skip(1).filter(|p| !p.is_empty()).count();
    let module_leaf = marker_only || (ctx.prefix.is_empty() && after_marker <= 1);
    if !module_leaf {
        return false;
    }
    if let Some((name, parent)) = full.split_last() {
        if !marker_only && !parent.is_empty() && (!maybe_external || raw.contains("::")) {
            fx.import_edges.push(
                ImportEdgeCandidate::symbol(ctx.line, parent.join("."), name.clone())
                    .with_maybe_external(maybe_external),
            );
        } else {
            fx.import_edges.push(
                ImportEdgeCandidate::module(ctx.line, qn.clone())
                    .with_maybe_external(maybe_external),
            );
        }
    }
    let binding = alias.clone().or_else(|| {
        // Bare `use self;` binds nothing; `use a::b::{self}` binds `b` so
        // scoped self imports keep their module alias.
        if node.kind() == "self" && ctx.prefix.is_empty() {
            None
        } else {
            full.last().cloned()
        }
    });
    if let Some(binding) = binding {
        if maybe_external {
            fx.bindings
                .maybe_external_aliases
                .insert((ctx.alias_scope.to_string(), binding.clone()));
        }
        if let Some((name, parent)) = full.split_last() {
            if parent.is_empty() || (maybe_external && !raw.contains("::")) {
                fx.bindings.aliases.push((
                    ctx.alias_scope.to_string(),
                    binding,
                    ImportTarget::ModuleRoot { path: qn },
                ));
            } else {
                fx.bindings.aliases.push((
                    ctx.alias_scope.to_string(),
                    binding,
                    ImportTarget::Symbol {
                        module: parent.join("."),
                        name: name.clone(),
                    },
                ));
            }
        }
    }
    true
}

/// Resolve one use-tree path node against the enclosing module scope.
/// `None` means the path is external/absolute and must not produce an edge.
pub(super) fn resolve_use_path(node: Node<'_>, ctx: UseTreeCtx<'_>) -> Option<(Vec<String>, bool)> {
    let raw = text(node, ctx.src);
    if raw.starts_with("::") {
        return None;
    }
    let mut parts = raw
        .split("::")
        .filter(|p| !p.is_empty())
        .map(str::to_string);
    let first = parts.next()?;
    let rest: Vec<String> = parts.collect();
    let extra_supers = rest.iter().take_while(|p| p.as_str() == "super").count();
    let (mut out, plain, maybe_ext) = first_segment_prefix(&first, extra_supers, ctx)?;
    if plain {
        out.push(first);
    }
    out.extend(rest.into_iter().skip(extra_supers));
    Some((out, maybe_ext))
}

/// Resolve the first `::` segment against the crate root, scope, or prefix.
/// Returns `(prefix_segments, append_first_as_plain, maybe_external)`.
fn first_segment_prefix(
    first: &str,
    extra_supers: usize,
    ctx: UseTreeCtx<'_>,
) -> Option<(Vec<String>, bool, bool)> {
    let split_root = |r: &str| -> Vec<String> {
        r.split('.')
            .filter(|p| !p.is_empty())
            .map(str::to_string)
            .collect()
    };
    let (out, plain, maybe_ext) = match first {
        "crate" => (split_root(ctx.crate_root?), false, false),
        _ if ctx.crate_name == Some(first) => (split_root(ctx.lib_root?), false, false),
        "self" if !ctx.prefix.is_empty() => (ctx.prefix.to_vec(), false, ctx.maybe_external),
        "self" => (split_root(ctx.scope), false, false),
        "super" if !ctx.prefix.is_empty() => (
            pop_segments(ctx.prefix.to_vec(), extra_supers),
            false,
            ctx.maybe_external,
        ),
        "super" => (
            pop_segments(split_root(ctx.scope), extra_supers),
            false,
            false,
        ),
        _ if ctx.prefix.is_empty() => (split_root(ctx.scope), true, true),
        _ => (ctx.prefix.to_vec(), true, ctx.maybe_external),
    };
    Some((out, plain, maybe_ext))
}

fn pop_segments(mut segments: Vec<String>, extra_supers: usize) -> Vec<String> {
    for _ in 0..=extra_supers {
        segments.pop();
    }
    segments
}
