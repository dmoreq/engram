//! Call-site resolution: raw name and attribute callee lookup, lexical
//! scope and alias binding searches, and edge/unresolved-site emission.

use super::*;

/// Resolve every recorded call site into an edge or an unresolved site.
pub(super) fn resolve_call_sites(
    ctx: EmitCtx<'_>,
    unresolved_per_file: &mut [usize],
    stats: &mut ExtractStats,
) {
    let mut seen_type_refs: HashSet<(usize, usize)> = ctx
        .t
        .edges
        .iter()
        .filter(|e| e.kind == EdgeKind::TypeRef)
        .filter_map(|e| e.dst_idx.map(|d| (e.src_idx, d)))
        .collect();
    // Call sites.
    for (fi, fctx) in ctx.t.file_ctx.iter().enumerate() {
        for site in &fctx.calls {
            let (outcome, receiver_class) = raw_resolve_call(ctx.t, fi, site);
            let src_idx = source_symbol_idx(ctx.t, fctx, site);
            // Type-only reference: a typed receiver `x.m()` depends on the
            // class of `x`, not just on the method the call resolves to.
            if let Some(class_qn) = receiver_class {
                if let Some(class_idx) = unique_class_index(ctx.t, &class_qn) {
                    if seen_type_refs.insert((src_idx, class_idx)) {
                        ctx.t.edges.push(NewEdge {
                            src_idx,
                            dst_idx: Some(class_idx),
                            dst_name: class_qn,
                            kind: EdgeKind::TypeRef,
                            confidence: Confidence::Extracted,
                            site_line: site.line,
                        });
                    }
                }
            }
            match outcome {
                Outcome::Extracted(dst) => {
                    let (dst_name, ctor) = extracted_edges(ctx.t, dst);
                    emit_extracted_call(&mut ctx.t.edges, src_idx, dst, dst_name, ctor, site.line);
                }
                Outcome::Ambiguous(name) => {
                    ctx.t.edges.push(ambiguous_edge(src_idx, name, site.line));
                }
                Outcome::Dropped => stats.builtin_dropped += 1,
                Outcome::Unresolved => {
                    stats.unresolved_callees += 1;
                    unresolved_per_file[fi] += 1;
                    ctx.unresolved_calls.push(UnresolvedSite {
                        file_idx: fi,
                        name: unresolved_site_name(site),
                        kind: EdgeSiteKind::Call,
                        site_line: site.line,
                    });
                }
            }
        }
    }
}

/// The `Outcome::Extracted` emit pair (R6d A6): keep the edge to the class
/// itself so `callers Class` and `dead` are correct, and add a second edge to
/// its constructor when unique. Like [`push_call_edge`], takes `edges`
/// separately from `Tables` because the caller iterates `t.file_ctx` while
/// pushing, which only borrows-split at field level.
pub(super) fn emit_extracted_call(
    edges: &mut Vec<NewEdge>,
    src_idx: usize,
    dst: usize,
    dst_name: String,
    ctor: Option<(usize, String)>,
    site_line: usize,
) {
    push_call_edge(edges, src_idx, Some(dst), dst_name, site_line);
    if let Some((ctor_idx, ctor_name)) = ctor {
        push_call_edge(edges, src_idx, Some(ctor_idx), ctor_name, site_line);
    }
}

/// Push the extracted-confidence call edge shared by the class edge and the
/// unique-constructor edge (R6d A6); the pair differs only in destination.
/// Takes `edges` separately from `Tables` because the caller iterates
/// `t.file_ctx` while pushing, which only borrows-split at field level.
pub(super) fn push_call_edge(
    edges: &mut Vec<NewEdge>,
    src_idx: usize,
    dst_idx: Option<usize>,
    dst_name: String,
    site_line: usize,
) {
    edges.push(NewEdge {
        src_idx,
        dst_idx,
        dst_name,
        kind: EdgeKind::Calls,
        confidence: Confidence::Extracted,
        site_line,
    });
}

pub(super) fn ambiguous_edge(src_idx: usize, name: String, site_line: usize) -> NewEdge {
    NewEdge {
        src_idx,
        dst_idx: None,
        dst_name: name,
        kind: EdgeKind::Calls,
        confidence: Confidence::Ambiguous,
        site_line,
    }
}

pub(super) fn source_symbol_idx(t: &Tables, fctx: &FileCtx, site: &CallSite) -> usize {
    if site.scope_func.is_empty() {
        return fctx.module_sym_idx;
    }
    // Two files can produce the same qualname (`a/b.py` and `a.b.py` both map
    // to `a.b`); the call belongs to THIS file, so filter candidates by the
    // file the enclosing module symbol lives in instead of taking the first.
    let file_idx = t.symbols[fctx.module_sym_idx].file_idx;
    t.by_qualname
        .get(&site.scope_func)
        .and_then(|v| {
            v.iter()
                .copied()
                .find(|&i| t.symbols[i].file_idx == file_idx)
        })
        .unwrap_or(fctx.module_sym_idx)
}

/// The class edge and, when the target is a class with a unique constructor,
/// the constructor edge. `None` when the class itself is the constructor.
pub(super) fn extracted_edges(t: &Tables, dst: usize) -> (String, Option<(usize, String)>) {
    let dst_name = t.symbols[dst].qualname.clone();
    let ctor = if t.symbols[dst].kind == SymbolKind::Class {
        ctor_target(t, dst)
            .filter(|c| *c != dst)
            .map(|c| (c, t.symbols[c].qualname.clone()))
    } else {
        None
    };
    (dst_name, ctor)
}

pub(super) fn unresolved_site_name(site: &CallSite) -> String {
    match &site.callee {
        Callee::Name(n) => n.clone(),
        Callee::Attr(parts) => parts.join("."),
    }
}

/// Unique constructor of a class (`__init__` / `constructor`), if any.
/// Instantiations emit edges to BOTH the class and this constructor (R6d A6).
pub(super) fn ctor_target(t: &Tables, class_idx: usize) -> Option<usize> {
    let class_qn = t.symbols[class_idx].qualname.to_string();
    for ctor in ["__init__", "constructor"] {
        match t.class_methods.get(&class_qn).and_then(|m| m.get(ctor)) {
            Some(inits) if inits.len() == 1 => return Some(inits[0]),
            Some(_) => return None, // ambiguous ctor: class edge only
            None => continue,
        }
    }
    None
}

/// Latest annotation or assignment at or before `site_line` in one scope.
/// `include_params` treats a parameter annotation as a binding at line 0.
pub(super) fn latest_binding<'a>(
    fctx: &'a FileCtx,
    scope: &str,
    var: &str,
    site_line: usize,
    include_params: bool,
) -> Option<&'a Vec<String>> {
    if let Some(events) = fctx
        .bindings
        .assignments
        .get(scope)
        .and_then(|m| m.get(var))
    {
        let idx = events.partition_point(|(line, _)| *line <= site_line);
        if idx > 0 {
            return Some(&events[idx - 1].1);
        }
    }
    if include_params {
        if let Some(ann) = fctx.bindings.params.get(scope).and_then(|m| m.get(var)) {
            return Some(ann);
        }
    }
    None
}

/// Receiver typing at a call site: the latest annotation or assignment at or
/// before `site_line`. The exact function scope wins; module scope is the
/// fallback. A later assignment never changes an earlier call.
pub(super) fn resolve_var_class(
    t: &Tables,
    fi: usize,
    var: &str,
    scope: &str,
    scope_class: Option<&str>,
    site_line: usize,
    depth: usize,
) -> Option<String> {
    let fctx = &t.file_ctx[fi];
    let mut scopes: Vec<(&str, bool)> = vec![(scope, true)];
    let mut parent = scope;
    while let Some((p, _)) = parent.rsplit_once('.') {
        scopes.push((p, true));
        parent = p;
    }
    if !scope.is_empty() {
        scopes.push(("", true));
    }
    for &(s, inc_params) in &scopes {
        if let Some(target) = latest_binding(fctx, s, var, site_line, inc_params) {
            if target.is_empty() {
                return None;
            }
            return resolve_target_class(t, fi, target, scope, scope_class, site_line, depth);
        }
    }
    None
}

/// Borrowed call-site context shared by the name/attribute resolvers.
pub(super) struct CallCtx<'a> {
    pub(super) t: &'a Tables,
    pub(super) fi: usize,
    pub(super) fctx: &'a FileCtx,
    pub(super) site: &'a CallSite,
    pub(super) depth: usize,
}

pub(super) fn raw_resolve_call(
    t: &Tables,
    fi: usize,
    site: &CallSite,
) -> (Outcome, Option<String>) {
    let ctx = CallCtx {
        t,
        fi,
        fctx: &t.file_ctx[fi],
        site,
        depth: 0,
    };
    match &site.callee {
        Callee::Name(name) => (resolve_name_callee(&ctx, name), None),
        Callee::Attr(chain) => resolve_attr_with_type_ref(&ctx, chain),
    }
}

pub(super) fn resolve_name_callee(ctx: &CallCtx<'_>, name: &str) -> Outcome {
    // 0. Lexical function scope: a nested def is visible to its
    // enclosing function and to deeper nested functions (closure).
    // Class scope is not lexical in Python, so stop before it.
    if let Some(out) = lexical_scope_lookup(ctx, name) {
        return out;
    }
    // 0b. Function-local import binding: a local `from a import f`
    // shadows the file-wide binding for this function.
    if let Some((alias_scope, target)) = nearest_alias_scoped(ctx.fctx, &ctx.site.scope_func, name)
    {
        if !alias_scope.is_empty() && ctx.site.scope_mod.as_deref() != Some(alias_scope) {
            return alias_target_outcome(ctx.t, target, name).unwrap_or(Outcome::Unresolved);
        }
    }
    // 0c. A parameter/local variable shadows module and file-level
    // names; it is not a static call target.
    if is_local_binding(ctx.fctx, ctx.site, name) {
        return Outcome::Unresolved;
    }
    // 1. Local module definition.
    if let Some(out) = bare_candidates(ctx.t, ctx.t.mod_child(&ctx.fctx.module, name), name, false)
    {
        return out;
    }
    // 2. Import alias map (follows re-export chains transitively).
    if let Some(out) = alias_binding_lookup(ctx, name) {
        return out;
    }
    // 3. Star imports: follow re-exports transitively with idx dedup.
    let lookup_scope = if ctx.site.scope_func.is_empty() {
        ctx.site.scope_mod.as_deref().unwrap_or("")
    } else {
        ctx.site.scope_func.as_str()
    };
    let mut star_hits: Vec<usize> = Vec::new();
    for (m, line) in &ctx.fctx.bindings.stars {
        let star_scope = ctx.fctx.bindings.star_scope(m, *line);
        if !scope_visible(star_scope, lookup_scope) {
            continue;
        }
        let mut seen: HashSet<(&str, &str)> = HashSet::new();
        if let Some(out) = follow_callable_binding(ctx.t, (m, name), 0, &mut seen, name) {
            match out {
                Outcome::Extracted(idx) if !star_hits.contains(&idx) => star_hits.push(idx),
                Outcome::Extracted(_) | Outcome::Dropped | Outcome::Unresolved => {}
                Outcome::Ambiguous(_) => return out,
            }
        }
    }
    match star_hits.len() {
        0 => {}
        1 => return Outcome::Extracted(star_hits[0]),
        _ => return Outcome::Ambiguous(name.to_string()),
    }
    // 4. Builtin table (scoped by file language).
    if is_builtin(ctx.fctx.lang, name) {
        return Outcome::Dropped;
    }
    Outcome::Unresolved
}

pub(super) fn lexical_scope_lookup(ctx: &CallCtx<'_>, name: &str) -> Option<Outcome> {
    if ctx.site.scope_func.is_empty() {
        return None;
    }
    let mut scope = ctx.site.scope_func.clone();
    while Some(scope.as_str()) != ctx.site.scope_class.as_deref() && scope != ctx.fctx.module {
        let candidate = format!("{scope}.{name}");
        if let Some(out) = bare_candidates(ctx.t, ctx.t.qualname_child(&candidate), name, false) {
            return Some(out);
        }
        let Some((parent, _)) = scope.rsplit_once('.') else {
            break;
        };
        if Some(parent) == ctx.site.scope_class.as_deref() || parent == ctx.fctx.module {
            break;
        }
        scope = parent.to_string();
    }
    None
}

pub(super) fn alias_binding_lookup(ctx: &CallCtx<'_>, name: &str) -> Option<Outcome> {
    let lookup_scope = if ctx.site.scope_func.is_empty() {
        ctx.site.scope_mod.as_deref().unwrap_or("")
    } else {
        ctx.site.scope_func.as_str()
    };
    let (_, target) = nearest_alias_scoped(ctx.fctx, lookup_scope, name)?;
    alias_target_outcome(ctx.t, target, name)
}

/// Shared alias resolution for the Symbol and ModuleRoot import targets:
/// follow the binding through re-exports, drop bindings to modules that are
/// not indexed, and try the same-named child for a root-level binding.
/// `None` leaves the fall-through value to the caller.
pub(super) fn alias_target_outcome(
    t: &Tables,
    target: &ImportTarget,
    name: &str,
) -> Option<Outcome> {
    match target {
        ImportTarget::Symbol { module, name: n } => {
            let mut seen: HashSet<(&str, &str)> = HashSet::new();
            if let Some(out) = follow_callable_binding(t, (module, n), 0, &mut seen, name) {
                return Some(out);
            }
            // Binding to a module that is not indexed at all = an
            // external library (react, pydantic, os.path, ...): drop.
            if !t.module_paths.contains(module) {
                return Some(Outcome::Dropped);
            }
            None
        }
        ImportTarget::ModuleRoot { path } => {
            bare_candidates(t, t.mod_child(path, name), name, false)
        }
        ImportTarget::Module { .. } => None,
    }
}

/// Resolve a binding chain to a class qualname: direct class lookup first,
/// then callable return-type lookup (`x = helper()`, `x = mod.helper()`).
pub(super) fn resolve_target_class(
    t: &Tables,
    fi: usize,
    chain: &[String],
    scope: &str,
    scope_class: Option<&str>,
    site_line: usize,
    depth: usize,
) -> Option<String> {
    if let Some(qn) = resolve_class_of_chain(t, fi, chain, scope) {
        return Some(qn);
    }
    if depth >= 2 || chain.is_empty() {
        return None;
    }
    let fctx = &t.file_ctx[fi];
    let class_qn = scope_class.map(str::to_string).or_else(|| {
        t.by_qualname
            .get(scope)
            .and_then(|v| {
                v.iter()
                    .copied()
                    .find(|&i| is_class_kind(t.symbols[i].kind))
            })
            .map(|i| t.symbols[i].qualname.clone())
            .or_else(|| {
                scope.rsplit_once('.').and_then(|(p, _)| {
                    t.by_qualname
                        .get(p)
                        .and_then(|v| {
                            v.iter()
                                .copied()
                                .find(|&i| is_class_kind(t.symbols[i].kind))
                        })
                        .map(|i| t.symbols[i].qualname.clone())
                })
            })
    });
    let synthetic = CallSite {
        line: site_line,
        scope_func: scope.to_string(),
        scope_class: class_qn,
        scope_mod: None,
        callee: if chain.len() == 1 {
            Callee::Name(chain[0].clone())
        } else {
            Callee::Attr(chain.to_vec())
        },
    };
    let ctx = CallCtx {
        t,
        fi,
        fctx,
        site: &synthetic,
        depth: depth + 1,
    };
    let outcome = if chain.len() == 1 {
        resolve_name_callee(&ctx, &chain[0])
    } else {
        resolve_attr_callee(&ctx, chain)
    };
    if let Outcome::Extracted(dst) = outcome {
        return t.return_types.get(&t.symbols[dst].qualname).cloned();
    }
    None
}

/// MRO-aware field / `@property` type lookup for `self.field` and `obj.field`.
pub(super) fn resolve_self_attr_type(t: &Tables, class_qn: &str, attr: &str) -> Option<String> {
    if let Some(target) = t.class_fields.get(class_qn).and_then(|m| m.get(attr)) {
        return Some(target.clone());
    }
    if let Some(idxs) = t.class_methods.get(class_qn).and_then(|m| m.get(attr)) {
        if idxs.len() == 1 {
            if let Some(ret) = t.return_types.get(&t.symbols[idxs[0]].qualname) {
                return Some(ret.clone());
            }
        }
    }
    let bases = t.class_bases.get(class_qn)?;
    let mut level = bases.clone();
    let mut seen: HashSet<String> = HashSet::from([class_qn.to_string()]);
    for _ in 0..5 {
        let mut next: Vec<String> = Vec::new();
        let mut found: Vec<String> = Vec::new();
        for qn in &level {
            if !seen.insert(qn.clone()) {
                continue;
            }
            if let Some(target) = t.class_fields.get(qn).and_then(|m| m.get(attr)) {
                found.push(target.clone());
            } else if let Some(idxs) = t.class_methods.get(qn).and_then(|m| m.get(attr)) {
                if idxs.len() == 1 {
                    if let Some(ret) = t.return_types.get(&t.symbols[idxs[0]].qualname) {
                        found.push(ret.clone());
                    }
                }
            } else if let Some(bases) = t.class_bases.get(qn) {
                next.extend(bases.iter().cloned());
            }
        }
        match found.len() {
            1 => return Some(found.into_iter().next().unwrap()),
            0 if next.is_empty() => return None,
            0 => level = next,
            _ => return None,
        }
    }
    None
}
