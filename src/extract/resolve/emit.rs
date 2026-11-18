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
                .entry((parent, name.to_string()))
                .or_default()
                .push(idx);
        }
        _ if direct_child && kind != SymbolKind::Method => {
            t.by_mod_name
                .entry((module.to_string(), name.to_string()))
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
    let base_qn = resolve_base_chain(ctx.t, class_file, chain, &src_qn);
    if let Some(qn) = base_qn {
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
    ctx.unresolved_calls.push(UnresolvedSite {
        file_idx: class_file,
        name: chain.join("."),
        kind: EdgeSiteKind::Inherit,
        site_line,
    });
    None
}
