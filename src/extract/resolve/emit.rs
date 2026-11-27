//! Inherit-edge emission and symbol table indexing: parent-module and
//! method-class registration plus ambiguous-inherit edge creation.

use super::*;

/// Index one symbol under its parent module (Python bare-name visibility).
/// Rust inline `mod` members are scoped as methods, but their parent is a
/// module; they are importable/callable bare names. The parent is already
/// in `by_qualname` (symbols are processed in order), so its kind is one
/// lookup instead of a reverse scan. The symbol's own fields are read back
pub(super) fn index_parent_module(t: &mut Tables, idx: usize, module: &str) {
    let sym = &t.symbols[idx];
    let (name, qualname, kind) = (sym.name.as_str(), sym.qualname.as_str(), sym.kind);
    let direct_child = is_direct_child(module, name, qualname);
    let parent_qn_kind = qualname.rsplit_once('.').map(|(parent, _)| {
        let parent_kind = t
            .by_qualname
            .get(parent)
            .and_then(|v| v.last())
            .map(|i| t.symbols[*i].kind);
        (parent.to_string(), parent_kind)
    });
    match parent_qn_kind {
        // Any direct child of a module is a bare-name candidate, even when
        // Rust inline `mod` scopes it as a method.
        Some((parent, Some(SymbolKind::Module))) => {
            t.by_mod_name
                .entry(parent)
                .or_default()
                .entry(name.to_string())
                .or_default()
                .push(idx);
        }
        // A child module whose parent module symbol is registered later (file
        // order): derive the parent from the qualname itself so `pkg.sub`
        // stays importable regardless of processing order.
        _ if kind == SymbolKind::Module && qualname.contains('.') => {
            let parent = qualname
                .rsplit_once('.')
                .map(|(p, _)| p.to_string())
                .unwrap_or_default();
            t.by_mod_name
                .entry(parent)
                .or_default()
                .entry(name.to_string())
                .or_default()
                .push(idx);
        }
        _ if direct_child && kind != SymbolKind::Method => {
            t.by_mod_name
                .entry(module.to_string())
                .or_default()
                .entry(name.to_string())
                .or_default()
                .push(idx);
        }
        _ => {}
    }
}

/// Index one method under its bare name and its class (the MRO walks), and
/// make sure every class qualname owns a method map.
pub(super) fn index_method_class(
    t: &mut Tables,
    idx: usize,
    name: &str,
    qualname: &str,
    kind: SymbolKind,
) {
    if kind == SymbolKind::Method {
        let class_qn = qualname
            .rsplit_once('.')
            .map(|(p, _)| p.to_string())
            .unwrap_or_default();
        t.methods_by_name
            .entry(name.to_string())
            .or_default()
            .push(idx);
        t.class_methods
            .entry(class_qn)
            .or_default()
            .entry(name.to_string())
            .or_default()
            .push(idx);
    }
    if kind == SymbolKind::Class {
        t.class_methods.entry(qualname.to_string()).or_default();
    }
}

/// The kinds that can carry an inheritance edge: classes, traits, and
/// interfaces share the base/MRO machinery.
pub(super) fn is_class_kind(kind: SymbolKind) -> bool {
    matches!(
        kind,
        SymbolKind::Class | SymbolKind::Trait | SymbolKind::Interface
    )
}

/// Borrowed emit context shared by the edge emitters: the resolver tables plus
/// the unresolved-site sink travel as one value (the CallCtx pattern) instead
/// of a loose `(&mut Tables, &mut Vec<UnresolvedSite>)` pair.
pub(super) struct EmitCtx<'a> {
    pub(super) t: &'a mut Tables,
    pub(super) unresolved_calls: &'a mut Vec<UnresolvedSite>,
}

/// Resolve one base chain and emit its `inherits` edge or its unresolved
/// site. Returns the resolved base qualname when the edge is unambiguous;
/// callers decide whether it joins the MRO table (`class_bases`: fresh
/// declarations do, interface implements lists do not).
pub(super) fn emit_inherit_edge(
    ctx: &mut EmitCtx<'_>,
    src_idx: usize,
    chain: &[String],
    site_line: usize,
) -> Option<String> {
    let class_file = ctx.t.symbols[src_idx].file_idx;
    let src_qn = ctx.t.symbols[src_idx].qualname.clone();
    match resolve_base_chain(ctx.t, class_file, chain, &src_qn) {
        BaseChain::Unique(qn) => {
            if let Some(dst) = unique_class_index(ctx.t, &qn) {
                ctx.t.edges.push(NewEdge {
                    src_idx,
                    dst_idx: Some(dst),
                    dst_name: qn.clone(),
                    kind: EdgeKind::Inherits,
                    confidence: Confidence::Extracted,
                    site_line,
                });
                return Some(qn);
            }
        }
        BaseChain::Ambiguous(qn) => {
            // Several class-kind symbols share the base qualname: keep the
            // relation visible as an AMBIGUOUS dst-less edge instead of
            // silently collapsing the site into a plain unresolved row.
            ctx.t.edges.push(NewEdge {
                src_idx,
                dst_idx: None,
                dst_name: qn.clone(),
                kind: EdgeKind::Inherits,
                confidence: Confidence::Ambiguous,
                site_line,
            });
        }
        BaseChain::NotFound => {}
    }
    ctx.unresolved_calls.push(UnresolvedSite {
        file_idx: class_file,
        name: chain.join("."),
        kind: EdgeSiteKind::Inherit,
        site_line,
    });
    None
}

/// MRO walk, level by level: fast-path level 0 with `&str` (zero allocs when
/// the class defines the method or has no bases); BFS up to 6 levels otherwise.
pub(super) fn mro_outcome(t: &Tables, class_qn: &str, method: &str) -> Outcome {
    if let Some(idxs) = t.class_methods.get(class_qn).and_then(|m| m.get(method)) {
        return match idxs.len() {
            1 => Outcome::Extracted(idxs[0]),
            0 => Outcome::Unresolved,
            _ => Outcome::Ambiguous(format!("{class_qn}.{method}")),
        };
    }
    let Some(bases) = t.class_bases.get(class_qn) else {
        return Outcome::Unresolved;
    };
    let mut level = bases.clone();
    let mut seen: HashSet<String> = HashSet::from([class_qn.to_string()]);
    for _ in 0..5 {
        let mut next: Vec<String> = Vec::new();
        let mut found: Vec<usize> = Vec::new();
        for qn in &level {
            if !seen.insert(qn.clone()) {
                continue;
            }
            if let Some(idxs) = t.class_methods.get(qn).and_then(|m| m.get(method)) {
                found.extend_from_slice(idxs);
            } else if let Some(bases) = t.class_bases.get(qn) {
                next.extend(bases.iter().cloned());
            }
        }
        match found.len() {
            1 => return Outcome::Extracted(found[0]),
            0 if next.is_empty() => return Outcome::Unresolved,
            0 => level = next,
            _ => return Outcome::Ambiguous(format!("{class_qn}.{method}")),
        }
    }
    Outcome::Unresolved
}

/// Resolve fresh return types, `self_fields`, and parameter annotations into
/// `Tables`, persisting `symbols.return_type`, `ResolvedClassField`s, and `TypeRef` edges.
pub(super) fn resolve_types_and_fields(t: &mut Tables) -> Vec<ResolvedClassField> {
    let mut out_fields = Vec::new();
    let mut type_ref_edges: Vec<NewEdge> = Vec::new();
    for fi in 0..t.file_ctx.len() {
        let fctx_file_idx = t.symbols[t.file_ctx[fi].module_sym_idx].file_idx;
        let ret_items = std::mem::take(&mut t.file_ctx[fi].bindings.return_types);
        for (func_qn, ret_chain) in ret_items {
            if let Some(class_qn) = resolve_class_of_chain(t, fi, &ret_chain, &func_qn) {
                t.return_types.insert(func_qn.clone(), class_qn.clone());
                let func_idx = t.by_qualname.get(&func_qn).and_then(|v| {
                    v.iter()
                        .copied()
                        .find(|&i| t.symbols[i].file_idx == fctx_file_idx)
                });
                if let Some(fidx) = func_idx {
                    t.symbols[fidx].return_type = class_qn.clone();
                    if let Some(cidx) = unique_class_index(t, &class_qn) {
                        type_ref_edges.push(NewEdge {
                            src_idx: fidx,
                            dst_idx: Some(cidx),
                            dst_name: class_qn,
                            kind: EdgeKind::TypeRef,
                            confidence: Confidence::Extracted,
                            site_line: t.symbols[fidx].line_start,
                        });
                    }
                }
            }
        }
        // Emit TypeRef edges for resolved function parameter annotations.
        let param_items: Vec<(String, Vec<Vec<String>>)> = t.file_ctx[fi]
            .bindings
            .params
            .iter()
            .map(|(fq, vars)| (fq.clone(), vars.values().cloned().collect()))
            .collect();
        for (func_qn, chains) in param_items {
            let Some(fidx) = t.by_qualname.get(&func_qn).and_then(|v| {
                v.iter()
                    .copied()
                    .find(|&i| t.symbols[i].file_idx == fctx_file_idx)
            }) else {
                continue;
            };
            let mut seen_classes: HashSet<usize> = HashSet::new();
            for ann_chain in chains {
                if let Some(class_qn) = resolve_class_of_chain(t, fi, &ann_chain, &func_qn) {
                    if let Some(cidx) = unique_class_index(t, &class_qn) {
                        if seen_classes.insert(cidx) {
                            type_ref_edges.push(NewEdge {
                                src_idx: fidx,
                                dst_idx: Some(cidx),
                                dst_name: class_qn,
                                kind: EdgeKind::TypeRef,
                                confidence: Confidence::Extracted,
                                site_line: t.symbols[fidx].line_start,
                            });
                        }
                    }
                }
            }
        }
    }
    for fi in 0..t.file_ctx.len() {
        let fctx_file_idx = t.symbols[t.file_ctx[fi].module_sym_idx].file_idx;
        let mod_sym_idx = t.file_ctx[fi].module_sym_idx;
        let sf_items = std::mem::take(&mut t.file_ctx[fi].bindings.self_fields);
        for sf in sf_items {
            if let Some(target_qn) = resolve_target_class(
                t,
                fi,
                &sf.target_dotted,
                &sf.class_qn,
                Some(&sf.class_qn),
                sf.site_line,
                0,
            ) {
                t.class_fields
                    .entry(sf.class_qn.clone())
                    .or_default()
                    .insert(sf.field.clone(), target_qn.clone());
                out_fields.push(ResolvedClassField {
                    file_idx: fctx_file_idx,
                    class_qualname: sf.class_qn.clone(),
                    field: sf.field,
                    target_qualname: target_qn.clone(),
                });
                if let Some(cidx) = unique_class_index(t, &target_qn) {
                    let src_idx = unique_class_index(t, &sf.class_qn).unwrap_or(mod_sym_idx);
                    type_ref_edges.push(NewEdge {
                        src_idx,
                        dst_idx: Some(cidx),
                        dst_name: target_qn,
                        kind: EdgeKind::TypeRef,
                        confidence: Confidence::Extracted,
                        site_line: sf.site_line,
                    });
                }
            }
        }
    }
    t.edges.extend(type_ref_edges);
    out_fields
}

pub(super) fn resolve_reexported_class(
    t: &Tables,
    module: &str,
    name: &str,
    tail: &[String],
) -> Option<String> {
    let qn = format!("{module}.{name}");
    let base_qn = if unique_class_exists(t, &qn) {
        Some(qn)
    } else {
        follow_reexport(t, module, name)
    }?;
    if tail.is_empty() {
        return Some(base_qn);
    }
    let nested = format!("{}.{}", base_qn, tail.join("."));
    unique_class_exists(t, &nested).then_some(nested)
}

/// Follow package re-export chains (`from pkg import Base` where pkg/__init__
/// re-exports it) to the defining class qualname.
pub(super) fn follow_reexport(t: &Tables, module: &str, name: &str) -> Option<String> {
    let mut seen: HashSet<(&str, &str)> = HashSet::new();
    match follow_binding(t, (module, name), 0, &mut seen, name) {
        Some(Outcome::Extracted(idx)) if is_class_kind(t.symbols[idx].kind) => {
            Some(t.symbols[idx].qualname.to_string())
        }
        _ => None,
    }
}
