//! Phase B: global symbol and edge resolution over all extracted files.
//!
//! Kept separate from extraction so the resolution rules (module trees, alias
//! chains, receiver typing, MRO) can be read without the AST walk.

use std::collections::{HashMap, HashSet};

use super::*;

/// (scope_func -> (var -> ordered (site_line, constructor target chain))).
type BindingEvents = HashMap<String, HashMap<String, Vec<(usize, Vec<String>)>>>;

struct FileCtx {
    module: String,
    lang: Lang,
    /// Crate-root dir segment for Rust files (e.g. "src") — maps crate:: paths.
    crate_root: Option<String>,
    module_sym_idx: usize,
    bindings: ResolvedBindings,
    unresolved_inherits: Vec<(String, usize)>,
    calls: Vec<CallSite>,
    import_edges: Vec<ImportEdgeCandidate>,
    /// (src idx global, base chain)
    inherits: Vec<(usize, Vec<String>)>,
    /// (src idx global, base chain, impl line) — edge-only, no class_bases.
    impl_inherits: Vec<(usize, Vec<String>, usize)>,
}

/// Resolution-time view of one file's [`FileBindings`] (extract.rs): the
/// per-symbol binding records folded into the lookup keys resolution reads
/// (`aliases`, `locals`, `assignments`, `params`, `self_fields`) plus the
/// records it consumes as-is (`arrow_params`, `stars`, `from_import_parents`).
/// Grouped with `FileBindings` so a new binding concept registers in one
/// block per side instead of per-field across `FileCtx` and `merge_fresh`.
struct ResolvedBindings {
    aliases: HashMap<String, Vec<(String, ImportTarget)>>,
    /// scope_func -> set of parameter/local names that shadow outer names.
    locals: HashMap<String, HashSet<String>>,
    /// Anonymous callback parameters, scoped by source line range.
    arrow_params: Vec<ArrowParam>,
    stars: Vec<(String, usize)>,
    star_scopes: HashMap<(String, usize), String>,
    maybe_external_stars: HashSet<(String, usize)>,
    maybe_external_aliases: HashSet<(String, String)>,
    from_import_parents: HashSet<(usize, String)>,
    /// scope_func -> (var -> ordered (site_line, constructor target chain)).
    assignments: BindingEvents,
    /// scope_func -> (var -> annotation chain).
    params: HashMap<String, HashMap<String, Vec<String>>>,
    /// Raw `self.field` / class-body field bindings, resolved in `resolve_types_and_fields`.
    self_fields: Vec<SelfField>,
    /// Raw function/method/`@property` return type chains, resolved in `resolve_types_and_fields`.
    return_types: Vec<(String, Vec<String>)>,
}

impl ResolvedBindings {
    pub(super) fn star_scope(&self, star_mod: &str, line: usize) -> &str {
        self.star_scopes
            .get(&(star_mod.to_string(), line))
            .map(String::as_str)
            .unwrap_or("")
    }
}

struct Tables {
    symbols: Vec<NewSymbol>,
    existing_ids: Vec<Option<i64>>,
    edges: Vec<NewEdge>,
    /// Direct module children only (module -> (name -> sym_idxs)).
    by_mod_name: HashMap<String, HashMap<String, Vec<usize>>>,
    by_qualname: HashMap<String, Vec<usize>>,
    class_methods: HashMap<String, HashMap<String, Vec<usize>>>,
    /// Every registered method index keyed by bare name, so the Rust
    /// unknown-receiver fallback is one lookup instead of a scan over all
    /// classes (`class_methods` keeps the per-class view for MRO walks).
    methods_by_name: HashMap<String, Vec<usize>>,
    /// class qualname -> resolved base qualnames
    class_bases: HashMap<String, Vec<String>>,
    /// class qualname -> (field -> resolved target class qualname)
    class_fields: HashMap<String, HashMap<String, String>>,
    /// function/method qualname -> resolved return class qualname
    return_types: HashMap<String, String>,
    /// Carried module re-exports: `module_qn -> (exported_name -> sym_idxs)`
    carried_reexports: HashMap<String, HashMap<String, Vec<usize>>>,
    /// Carried star re-exports: `module_qn -> [star_module]`
    carried_stars: HashMap<String, Vec<String>>,
    module_paths: HashSet<String>,
    file_ctx: Vec<FileCtx>,
    /// module -> file_ctx index (first fresh file wins, matching the linear
    /// search it replaced), for the re-export binding walk.
    file_by_module: HashMap<String, usize>,
}

impl Tables {
    pub(super) fn qualname_child(&self, qualname: &str) -> Option<&[usize]> {
        self.by_qualname.get(qualname).map(|v| v.as_slice())
    }

    pub(super) fn mod_child(&self, module: &str, name: &str) -> Option<&[usize]> {
        self.by_mod_name
            .get(module)
            .and_then(|m| m.get(name))
            .map(|v| v.as_slice())
    }
}

enum Outcome {
    Extracted(usize),
    Ambiguous(String),
    Dropped,
    Unresolved,
}

/// Register one symbol in every resolution table. Shared by the fresh and
/// carried merge paths; `existing_id` is `Some(row_id)` for a carried row and
/// `None` for a freshly extracted one.
fn register_symbol(t: &mut Tables, sym: NewSymbol, module: &str, existing_id: Option<i64>) {
    let idx = t.symbols.len();
    let qualname = sym.qualname.clone();
    let kind = sym.kind;
    let name = sym.name.clone();
    if kind == SymbolKind::Module {
        t.module_paths.insert(qualname.clone());
    }
    t.symbols.push(sym);
    t.existing_ids.push(existing_id);
    t.by_qualname.entry(qualname.clone()).or_default().push(idx);
    index_parent_module(t, idx, module);
    index_method_class(t, idx, &name, &qualname, kind);
}
/// Phase-ordered resolver. Every phase mutates the same `Tables`, and the
/// intermediate state has no meaning outside this function; the phase comments
/// below are the navigational boundary.
pub fn resolve(mut files: Vec<FileExtraction>, carried: CarriedIndex) -> Resolution {
    let mut t = Tables {
        symbols: Vec::new(),
        existing_ids: Vec::new(),
        edges: Vec::new(),
        by_mod_name: HashMap::new(),
        by_qualname: HashMap::new(),
        class_methods: HashMap::new(),
        methods_by_name: HashMap::new(),
        class_bases: HashMap::new(),
        class_fields: HashMap::new(),
        return_types: HashMap::new(),
        carried_reexports: HashMap::new(),
        carried_stars: HashMap::new(),
        module_paths: HashSet::new(),
        file_ctx: Vec::new(),
        file_by_module: HashMap::new(),
    };
    merge_fresh(&mut t, &mut files);
    merge_carried(&mut t, carried);
    prune_external_rust_imports(&mut t);
    let mut unresolved_calls = seed_unresolved_inherits(&t);
    emit_inherit_phase(EmitCtx {
        t: &mut t,
        unresolved_calls: &mut unresolved_calls,
    });
    let mut stats = ExtractStats::default();
    let mut unresolved_per_file = vec![0usize; t.file_ctx.len()];
    emit_import_edges(EmitCtx {
        t: &mut t,
        unresolved_calls: &mut unresolved_calls,
    });
    dedup_import_edges(&mut t, &mut unresolved_calls);
    reconcile_python_imports(&mut t);
    let class_fields = resolve_types_and_fields(&mut t);
    resolve_call_sites(
        EmitCtx {
            t: &mut t,
            unresolved_calls: &mut unresolved_calls,
        },
        &mut unresolved_per_file,
        &mut stats,
    );
    let (module_exports, module_stars) = collect_module_exports(&t);
    Resolution {
        symbols: t.symbols,
        existing_ids: t.existing_ids,
        edges: t.edges,
        class_fields,
        module_exports,
        module_stars,
        stats,
        unresolved_per_file,
        unresolved_calls,
    }
}

fn collect_module_exports(t: &Tables) -> (Vec<ResolvedModuleExport>, Vec<ResolvedModuleStar>) {
    let mut exports = Vec::new();
    let mut stars = Vec::new();
    for fctx in &t.file_ctx {
        let file_idx = t.symbols[fctx.module_sym_idx].file_idx;
        for (star_mod, line) in &fctx.bindings.stars {
            let scope = fctx.bindings.star_scope(star_mod, *line);
            if (scope.is_empty() || scope == fctx.module) && star_mod != &fctx.module {
                stars.push(ResolvedModuleStar {
                    file_idx,
                    module_qualname: fctx.module.clone(),
                    star_module: star_mod.clone(),
                });
            }
        }
        for (alias, entries) in &fctx.bindings.aliases {
            for (scope, target) in entries {
                if !scope.is_empty() && scope != &fctx.module {
                    continue;
                }
                if let ImportTarget::Symbol { module, name } = target {
                    let mut seen: HashSet<(&str, &str)> = HashSet::new();
                    if let Some(Outcome::Extracted(dst_idx)) =
                        follow_binding(t, (module, name), 0, &mut seen, alias)
                    {
                        exports.push(ResolvedModuleExport {
                            file_idx,
                            module_qualname: fctx.module.clone(),
                            exported_name: alias.clone(),
                            target_sym_idx: dst_idx,
                        });
                    }
                }
            }
        }
    }
    (exports, stars)
}

/// Merge fresh extractions: register symbols and build each file's
/// resolution context in one pass.
fn merge_fresh(t: &mut Tables, files: &mut Vec<FileExtraction>) {
    for mut fx in files.drain(..) {
        let base = t.symbols.len();
        let module = fx.module.clone();
        t.module_paths.insert(module.clone());
        for s in fx.symbols {
            register_symbol(t, s, &module, None);
        }
        let inherits = fx
            .inherits
            .into_iter()
            .map(|(src, chain)| (src + base, chain))
            .collect();
        let impl_inherits = fx
            .impl_inherits
            .into_iter()
            .map(|(src, chain, line)| (src + base, chain, line))
            .collect();
        let file_idx = t.file_ctx.len();
        // First file wins: carried symbols carry no FileCtx, so the module
        // -> fresh-file map mirrors the old linear `position()` search.
        t.file_by_module.entry(module.clone()).or_insert(file_idx);
        t.file_ctx.push(FileCtx {
            module,
            lang: fx.lang,
            crate_root: fx.crate_root,
            module_sym_idx: base,
            bindings: fold_binding_maps(&mut fx.bindings),
            unresolved_inherits: fx.unresolved_inherits,
            calls: fx.calls,
            import_edges: fx.import_edges,
            inherits,
            impl_inherits,
        });
    }
}

/// Fold the per-symbol binding records into the lookup keys resolution uses.
/// One block move out of [`FileBindings`]; the folded maps replace the raw
/// vectors and the pass-through records keep their order.
fn fold_binding_maps(fx: &mut FileBindings) -> ResolvedBindings {
    let bindings = std::mem::take(fx);
    let mut aliases: HashMap<String, Vec<(String, ImportTarget)>> = HashMap::new();
    for (scope, alias, target) in bindings.aliases {
        aliases.entry(alias).or_default().push((scope, target));
    }
    let mut locals: HashMap<String, HashSet<String>> = HashMap::new();
    for (scope, var) in bindings.locals {
        locals.entry(scope).or_default().insert(var);
    }
    let mut assignments: BindingEvents = HashMap::new();
    for a in bindings.assignments {
        assignments
            .entry(a.scope_func)
            .or_default()
            .entry(a.var)
            .or_default()
            .push((a.site_line, a.target_dotted));
    }
    for vars in assignments.values_mut() {
        for events in vars.values_mut() {
            events.sort_by_key(|(line, _)| *line);
        }
    }
    let mut params: HashMap<String, HashMap<String, Vec<String>>> = HashMap::new();
    for p in bindings.params {
        params
            .entry(p.scope_func)
            .or_default()
            .insert(p.var, p.ann_dotted);
    }
    ResolvedBindings {
        aliases,
        locals,
        arrow_params: bindings.arrow_params,
        stars: bindings.stars,
        star_scopes: bindings.star_scopes,
        maybe_external_stars: bindings.maybe_external_stars,
        maybe_external_aliases: bindings.maybe_external_aliases,
        from_import_parents: bindings.from_import_parents,
        assignments,
        params,
        self_fields: bindings.self_fields,
        return_types: bindings.return_types,
    }
}

fn is_indexed_rust_import(module_paths: &HashSet<String>, target: &ImportTarget) -> bool {
    match target {
        ImportTarget::Module { path } | ImportTarget::ModuleRoot { path } => {
            module_paths.contains(path)
        }
        ImportTarget::Symbol { module, name } => {
            module_paths.contains(module) || module_paths.contains(&format!("{module}.{name}"))
        }
    }
}

fn prune_external_rust_imports(t: &mut Tables) {
    for fctx in &mut t.file_ctx {
        if fctx.lang != Lang::Rust {
            continue;
        }
        fctx.import_edges
            .retain(|e| !e.maybe_external || is_indexed_rust_import(&t.module_paths, &e.target));
        let ext_stars = std::mem::take(&mut fctx.bindings.maybe_external_stars);
        if !ext_stars.is_empty() {
            fctx.bindings.stars.retain(|(m, line)| {
                !ext_stars.contains(&(m.clone(), *line)) || t.module_paths.contains(m)
            });
        }
        let ext_aliases = std::mem::take(&mut fctx.bindings.maybe_external_aliases);
        if !ext_aliases.is_empty() {
            for (alias, entries) in &mut fctx.bindings.aliases {
                entries.retain(|(scope, target)| {
                    !ext_aliases.contains(&(scope.clone(), alias.clone()))
                        || is_indexed_rust_import(&t.module_paths, target)
                });
            }
            fctx.bindings
                .aliases
                .retain(|_, entries| !entries.is_empty());
        }
    }
}

/// Merge carried symbols (fast path): join the resolution tables; their rows
/// are already stored and are never re-inserted.
fn merge_carried(t: &mut Tables, carried: CarriedIndex) {
    let CarriedIndex {
        symbols: carried_syms,
        inherits: carried_inherits,
        class_fields: carried_fields,
        reexports: carried_reexports,
        module_stars: carried_stars,
    } = carried;
    let mut row_to_idx: HashMap<i64, usize> = HashMap::with_capacity(carried_syms.len());
    for c in carried_syms {
        t.module_paths.insert(c.module.clone());
        // Carried rows are never re-extracted, so they have no fresh file:
        // usize::MAX marks them. The only read is the `file_idx != fi` skip
        // in `rust_unknown_receiver_outcome`; it is never used as a `file_ctx`
        // index, and the assert keeps it distinct from any real file index.
        debug_assert!(
            t.file_ctx.len() < usize::MAX,
            "carried file_idx sentinel collided with a real file index"
        );
        if !c.return_type.is_empty() {
            t.return_types
                .insert(c.qualname.clone(), c.return_type.clone());
        }
        let sym_idx = t.symbols.len();
        row_to_idx.insert(c.row_id, sym_idx);
        let sym = NewSymbol {
            kind: c.kind,
            name: c.name.clone(),
            qualname: c.qualname.clone(),
            file_idx: usize::MAX,
            line_start: 0,
            line_end: 0,
            signature: String::new(),
            return_type: c.return_type,

            ..Default::default()
        };
        register_symbol(t, sym, &c.module, Some(c.row_id));
    }
    for (sub_qn, base_qn) in carried_inherits {
        t.class_bases.entry(sub_qn).or_default().push(base_qn);
    }
    for (class_qn, field, target_qn) in carried_fields {
        t.class_fields
            .entry(class_qn)
            .or_default()
            .insert(field, target_qn);
    }
    for (mod_qn, exported_name, dst_row_id) in carried_reexports {
        if let Some(&sym_idx) = row_to_idx.get(&dst_row_id) {
            t.carried_reexports
                .entry(mod_qn)
                .or_default()
                .entry(exported_name)
                .or_default()
                .push(sym_idx);
        }
    }
    for (mod_qn, star_mod) in carried_stars {
        t.carried_stars.entry(mod_qn).or_default().push(star_mod);
    }
}

/// Seed the unresolved-site list from each file's unresolved base chains.
fn seed_unresolved_inherits(t: &Tables) -> Vec<UnresolvedSite> {
    let mut unresolved_calls: Vec<UnresolvedSite> = Vec::new();
    for (fi, fctx) in t.file_ctx.iter().enumerate() {
        for (name, site_line) in &fctx.unresolved_inherits {
            unresolved_calls.push(UnresolvedSite {
                file_idx: fi,
                name: name.clone(),
                kind: EdgeSiteKind::Inherit,
                site_line: *site_line,
            });
        }
    }
    unresolved_calls
}

/// Resolve fresh base chains and emit inherits edges (carried rows were
/// carried as edges). Per-file order (inherits, then impl_inherits) is kept.
fn emit_inherit_phase(mut ctx: EmitCtx<'_>) {
    // Resolve fresh base chains now that alias tables exist; emit inherits
    // edges for fresh classes (carried inherits edges were carried as rows).
    // Take each file's lists so the emitter can own `&mut Tables` without a
    // whole-corpus snapshot; per-file order (inherits, then impl_inherits)
    // is preserved and nothing re-reads these fields afterwards.
    for fi in 0..ctx.t.file_ctx.len() {
        let (inherits, impls) = {
            let f = &mut ctx.t.file_ctx[fi];
            (
                std::mem::take(&mut f.inherits),
                std::mem::take(&mut f.impl_inherits),
            )
        };
        for (src_idx, chain) in &inherits {
            let site = ctx.t.symbols[*src_idx].line_start;
            // Fresh class declarations populate the MRO table.
            if let Some(base_qn) = emit_inherit_edge(&mut ctx, *src_idx, chain, site) {
                let src_qn = ctx.t.symbols[*src_idx].qualname.clone();
                ctx.t.class_bases.entry(src_qn).or_default().push(base_qn);
            }
        }
        for (src_idx, chain, line) in &impls {
            // Interface implements lists resolve edges but stay out of the MRO
            // table.
            emit_inherit_edge(&mut ctx, *src_idx, chain, *line);
        }
    }
}

/// Resolve import candidates into edges (or unresolved sites) and update the
mod attr;
mod bindings;
mod calls;
mod emit;
mod imports;
#[cfg(test)]
mod tests;

use attr::*;
use bindings::*;
use calls::*;
use emit::*;
use imports::*;
