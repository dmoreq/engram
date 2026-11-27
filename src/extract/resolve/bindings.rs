//! Binding and class-chain resolution: alias following, MRO lookup,
//! variable/class chain typing, reexport chasing, and module-chain outcomes.

use super::*;

/// One `from m import n` binding under resolution: `(module, name)`. The same
/// pair doubles as the `seen` cycle-guard key, so the two values travel as one.
pub(super) type Binding<'a> = (&'a str, &'a str);

/// Follow `from m import n` bindings transitively through re-export chains
/// (module m has no def of n but itself imports it from elsewhere), cycle-safe.
pub(super) fn follow_binding<'a>(
    t: &'a Tables,
    binding: Binding<'a>,
    depth: usize,
    seen: &mut HashSet<Binding<'a>>,
    display: &str,
) -> Option<Outcome> {
    follow_binding_inner(t, binding, depth, seen, display, true)
}

pub(super) fn follow_callable_binding<'a>(
    t: &'a Tables,
    binding: Binding<'a>,
    depth: usize,
    seen: &mut HashSet<Binding<'a>>,
    display: &str,
) -> Option<Outcome> {
    follow_binding_inner(t, binding, depth, seen, display, false)
}

fn follow_binding_inner<'a>(
    t: &'a Tables,
    binding: Binding<'a>,
    depth: usize,
    seen: &mut HashSet<Binding<'a>>,
    display: &str,
    allow_module: bool,
) -> Option<Outcome> {
    let (module, name) = binding;
    if !seen.insert((module, name)) {
        return None;
    }
    if let Some(out) = bare_candidates(t, t.mod_child(module, name), display, allow_module) {
        return Some(out);
    }
    // Bounded re-export walk: a chain longer than 4 hops stays UNRESOLVED.
    // The cap is deliberate (bounded work per call site); `eg unresolved`
    // reports the resulting gaps rather than hiding them.
    if depth >= 4 {
        return None;
    }
    let file_idx = t.file_by_module.get(module).copied().or_else(|| {
        // Binary-only Rust crate (`src/main.rs` without `src/lib.rs`).
        let main_mod = main_module_for(module);
        t.file_by_module.get(&main_mod).copied()
    });
    let Some(file_idx) = file_idx else {
        if let Some(cands) = t.carried_reexports.get(module).and_then(|m| m.get(name)) {
            return bare_candidates(t, Some(cands.as_slice()), display, allow_module);
        }
        if let Some(stars) = t.carried_stars.get(module) {
            return star_follow(t, stars, name, depth, seen, display, allow_module);
        }
        return None;
    };
    let fctx = &t.file_ctx[file_idx];
    // Look the binding up under the file's own module scope: inline `mod`
    // `use` items record under it, and file-level imports record under ""
    // (always visible).
    if let Some(ImportTarget::Symbol {
        module: m2,
        name: n2,
    }) = nearest_alias_scoped(fctx, &fctx.module, name).map(|(_, target)| target)
    {
        return follow_binding_inner(t, (m2, n2), depth + 1, seen, display, allow_module);
    }
    // Star-import re-export fallback (`from .sub import *` / `export * from './sub'`).
    let mut star_hits: Vec<usize> = Vec::new();
    for (star_mod, line) in &fctx.bindings.stars {
        let star_scope = fctx.bindings.star_scope(star_mod, *line);
        if (!star_scope.is_empty() && star_scope != fctx.module) || star_mod == &fctx.module {
            continue;
        }
        if let Some(out) =
            follow_binding_inner(t, (star_mod, name), depth + 1, seen, display, allow_module)
        {
            match out {
                Outcome::Extracted(idx) if !star_hits.contains(&idx) => star_hits.push(idx),
                Outcome::Extracted(_) | Outcome::Dropped | Outcome::Unresolved => {}
                Outcome::Ambiguous(_) => return Some(out),
            }
        }
    }
    match star_hits.len() {
        0 => None,
        1 => Some(Outcome::Extracted(star_hits[0])),
        _ => Some(Outcome::Ambiguous(display.to_string())),
    }
}

fn star_follow<'a>(
    t: &'a Tables,
    stars: &'a [String],
    name: &'a str,
    depth: usize,
    seen: &mut HashSet<Binding<'a>>,
    display: &str,
    allow_module: bool,
) -> Option<Outcome> {
    let mut hits: Vec<usize> = Vec::new();
    for star_mod in stars {
        if let Some(out) =
            follow_binding_inner(t, (star_mod, name), depth + 1, seen, display, allow_module)
        {
            match out {
                Outcome::Extracted(idx) if !hits.contains(&idx) => hits.push(idx),
                Outcome::Extracted(_) | Outcome::Dropped | Outcome::Unresolved => {}
                Outcome::Ambiguous(_) => return Some(out),
            }
        }
    }
    match hits.len() {
        0 => None,
        1 => Some(Outcome::Extracted(hits[0])),
        _ => Some(Outcome::Ambiguous(display.to_string())),
    }
}

/// Resolve a dotted chain to a class qualname via: local module -> alias map ->
/// absolute qualname.
pub(super) fn resolve_base_chain(
    t: &Tables,
    fi: usize,
    chain: &[String],
    inheriting_qn: &str,
) -> BaseChain {
    let chain_str = chain.join(".");
    // File-local alias wins first: `use pkg::Type as T` inside pkg's own
    // module must follow the alias before the ancestor-package walk tries
    // `pkg.T`, which can bind a different, unrelated symbol. The lookup scope
    // is the file's module: top-level Rust `use` items record under it, and
    // Python file-level imports record under "" (always visible).
    if let Some(qn) = alias_class(t, &t.file_ctx[fi], chain, &t.file_ctx[fi].module) {
        return match class_match_count(t, &qn) {
            1 => BaseChain::Unique(qn),
            count if count > 1 => BaseChain::Ambiguous(qn),
            _ => BaseChain::NotFound,
        };
    }
    let mut parent = inheriting_qn
        .rsplit_once('.')
        .map(|(p, _)| p)
        .unwrap_or_default();
    loop {
        let candidate = if parent.is_empty() {
            chain_str.clone()
        } else {
            format!("{parent}.{chain_str}")
        };
        match class_match_count(t, &candidate) {
            1 => return BaseChain::Unique(candidate),
            count if count > 1 => return BaseChain::Ambiguous(candidate),
            _ => {}
        }
        match parent.rsplit_once('.') {
            Some((p, _)) => parent = p,
            None => break,
        }
    }
    match resolve_class_of_chain(t, fi, chain, &t.file_ctx[fi].module) {
        Some(qn) if class_match_count(t, &qn) > 1 => BaseChain::Ambiguous(qn),
        Some(qn) => BaseChain::Unique(qn),
        None => BaseChain::NotFound,
    }
}

/// Base-chain outcome for inheritance: unique candidates join the MRO, and a
/// multi-match candidate keeps a visible (dst-less, AMBIGUOUS) edge instead of
/// collapsing into a bare unresolved site.
pub(super) enum BaseChain {
    Unique(String),
    Ambiguous(String),
    NotFound,
}

pub(super) fn resolve_class_of_chain(
    t: &Tables,
    fi: usize,
    chain: &[String],
    scope: &str,
) -> Option<String> {
    if chain.is_empty() {
        return None;
    }
    let fctx = &t.file_ctx[fi];
    // `crate::Trait` in an impl target: map through the crate root first.
    if chain[0] == "crate" {
        if let Some(qn) = crate_class(t, fctx.crate_root.as_deref(), chain) {
            return Some(qn);
        }
    }
    // 1. Local module class (possibly nested: a.B.C).
    let local = format!("{}.{}", fctx.module, chain.join("."));
    if unique_class_exists(t, &local) {
        return Some(local);
    }
    // 2. Alias map.
    if let Some(qn) = alias_class(t, fctx, chain, scope) {
        return Some(qn);
    }
    // 2b. Star imports: `use super::*;` / `from mod import *` for classes/traits.
    let mut star_hits: Vec<String> = Vec::new();
    for (star_mod, line) in &fctx.bindings.stars {
        if !scope_visible(fctx.bindings.star_scope(star_mod, *line), scope) {
            continue;
        }
        if let Some(qn) = resolve_reexported_class(t, star_mod, &chain[0], &chain[1..]) {
            if !star_hits.contains(&qn) {
                star_hits.push(qn);
            }
        }
    }
    if star_hits.len() == 1 {
        return star_hits.into_iter().next();
    }
    // 3. Absolute qualname.
    let abs = chain.join(".");
    unique_class_exists(t, &abs).then_some(abs)
}

pub(super) fn crate_class(t: &Tables, root: Option<&str>, chain: &[String]) -> Option<String> {
    let root = root?;
    let mapped = if root.is_empty() {
        chain[1..].to_vec()
    } else {
        root_chain(root, chain)
    };
    if mapped.is_empty() {
        return None;
    }
    let qn = mapped.join(".");
    if unique_class_exists(t, &qn) {
        return Some(qn);
    }
    if chain.len() >= 2 {
        if let Some(reexp) = follow_reexport(t, root, &chain[1]) {
            if chain.len() == 2 {
                return Some(reexp);
            }
            let nested = format!("{reexp}.{}", chain[2..].join("."));
            if unique_class_exists(t, &nested) {
                return Some(nested);
            }
        }
    }
    if !t.file_by_module.contains_key(root) && chain.len() >= 2 {
        let main_mod = main_module_for(root);
        let main_qn = format!("{main_mod}.{}", chain[1..].join("."));
        if unique_class_exists(t, &main_qn) {
            return Some(main_qn);
        }
    }
    None
}

pub(super) fn alias_class(
    t: &Tables,
    fctx: &FileCtx,
    chain: &[String],
    scope: &str,
) -> Option<String> {
    let target = nearest_alias_scoped(fctx, scope, &chain[0]).map(|(_, target)| target)?;
    match target {
        ImportTarget::Symbol { module, name } => {
            resolve_reexported_class(t, module, name, &chain[1..])
        }
        ImportTarget::ModuleRoot { path } => {
            let (module, rest) = longest_module_prefix(t, path, &chain[1..])?;
            if rest.is_empty() {
                return None;
            }
            resolve_reexported_class(t, &module, &rest[0], &rest[1..])
        }
        ImportTarget::Module { .. } => None,
    }
}

pub(super) fn unique_class_exists(t: &Tables, qualname: &str) -> bool {
    unique_class_index(t, qualname).is_some()
}

/// Number of class-kind symbols sharing one qualname (0 when absent).
pub(super) fn class_match_count(t: &Tables, qualname: &str) -> usize {
    t.by_qualname.get(qualname).map_or(0, |v| {
        v.iter()
            .filter(|&&i| is_class_kind(t.symbols[i].kind))
            .count()
    })
}

/// The single class/trait/interface index for a qualname, when exactly one
/// exists. The `is_some()` of this function is also the existence test.
pub(super) fn unique_class_index(t: &Tables, qualname: &str) -> Option<usize> {
    t.by_qualname.get(qualname).and_then(|v| {
        let mut classes = v
            .iter()
            .copied()
            .filter(|&i| is_class_kind(t.symbols[i].kind));
        let first = classes.next()?;
        classes.next().is_none().then_some(first)
    })
}

pub(super) fn class_exists(t: &Tables, qualname: &str) -> bool {
    t.by_qualname
        .get(qualname)
        .is_some_and(|v| v.iter().any(|&i| is_class_kind(t.symbols[i].kind)))
}

/// Walk the module tree: consume chain segments while the prefix names an
/// indexed module; return (module, remaining segments).
pub(super) fn longest_module_prefix(
    t: &Tables,
    start: &str,
    rest: &[String],
) -> Option<(String, Vec<String>)> {
    if !t.module_paths.contains(start) {
        // PEP 420 namespace root (R6d I13): the package itself has no
        // __init__.py module symbol, but a deeper module may be indexed
        // (pkg -> pkg.mod). Resolve against the longest indexed prefix.
        let mut probe = start.to_string();
        let mut best: Option<(String, usize)> = None;
        for (i, seg) in rest.iter().enumerate() {
            probe = format!("{probe}.{seg}");
            if t.module_paths.contains(&probe) {
                best = Some((probe.clone(), i + 1));
            }
        }
        let (m, k) = best?;
        return Some((m, rest[k..].to_vec()));
    }
    let mut cur = start.to_string();
    let mut consumed = 0usize;
    for seg in rest {
        let next = format!("{cur}.{seg}");
        if t.module_paths.contains(&next) {
            cur = next;
            consumed += 1;
        } else {
            break;
        }
    }
    Some((cur, rest[consumed..].to_vec()))
}

/// `module.func(...)`, `pkg.mod.Class.m(...)` — resolve through the module tree.
pub(super) fn module_chain_outcome(
    t: &Tables,
    fi: usize,
    chain: &[String],
    scope: &str,
) -> Outcome {
    let fctx = &t.file_ctx[fi];
    let lang = fctx.lang;
    let start: Option<String> =
        match nearest_alias_scoped(fctx, scope, &chain[0]).map(|(_, target)| target) {
            Some(ImportTarget::ModuleRoot { path }) => {
                return chain_from_module(t, lang, path.to_string(), &chain[1..]);
            }
            Some(ImportTarget::Module { .. }) => None,
            Some(ImportTarget::Symbol { module, name }) => {
                let qn = format!("{module}.{name}");
                if t.module_paths.contains(&qn) {
                    return chain_from_module(t, lang, qn, &chain[1..]);
                }
                // Imported class: ImportedClass.method(...)
                if class_exists(t, &qn) && chain.len() == 2 {
                    return mro_outcome(t, &qn, &chain[1]);
                }
                // Re-exported namespace (`export * as ns from './sub'`): follow
                // the target module's alias when it points to a ModuleRoot.
                if let Some(&target_fi) = t.file_by_module.get(module) {
                    let target_fctx = &t.file_ctx[target_fi];
                    if let Some((_, ImportTarget::ModuleRoot { path })) =
                        nearest_alias_scoped(target_fctx, &target_fctx.module, name)
                    {
                        return chain_from_module(t, lang, path.clone(), &chain[1..]);
                    }
                }
                // An imported symbol whose origin module is not indexed
                // (third-party or stdlib package) is an external call site,
                // not this repo's unresolved gap.
                if !t.module_paths.contains(module) || is_stdlib_module(lang, module) {
                    return Outcome::Dropped;
                }
                return Outcome::Unresolved;
            }
            None => {
                if t.module_paths.contains(&chain[0]) {
                    Some(chain[0].clone())
                } else {
                    None
                }
            }
        };
    let Some(start_module) = start else {
        return Outcome::Unresolved;
    };
    chain_from_module(t, lang, start_module, &chain[1..])
}

pub(super) fn chain_from_module(
    t: &Tables,
    lang: Lang,
    module: String,
    rest: &[String],
) -> Outcome {
    if rest.is_empty() {
        return Outcome::Unresolved; // calling a bare module
    }
    if let Some((m, r)) = longest_module_prefix(t, &module, rest) {
        if r.is_empty() {
            // If a submodule name shadows a same-named callable or re-export in
            // its parent (e.g. `pkg/foo.py` vs `pkg/__init__.py`'s `from .foo import foo`),
            // fall back to `(parent_module, last_segment)` via non-module binding.
            if let Some((parent_mod, _)) = m.rsplit_once('.') {
                let last_seg = rest.last().unwrap();
                let mut seen: HashSet<(&str, &str)> = HashSet::new();
                if let Some(out) = follow_callable_binding(
                    t,
                    (parent_mod, last_seg.as_str()),
                    0,
                    &mut seen,
                    last_seg,
                ) {
                    return out;
                }
            }
            return Outcome::Unresolved;
        }
        if r.len() == 1 {
            if let Some(out) = bare_candidates(t, t.mod_child(&m, &r[0]), &r[0], false) {
                return out;
            }
            // Module-qualified re-exports (`pub use inner::helper` in m's
            // mod file): chase the binding chain when the direct child
            // lookup misses, so the call resolves to the re-exported symbol.
            let mut seen: HashSet<(&str, &str)> = HashSet::new();
            if let Some(out) =
                follow_callable_binding(t, (m.as_str(), r[0].as_str()), 0, &mut seen, &r[0])
            {
                return out;
            }
        }
        // Dotted path into the module: module.func or module.Class.method.
        let full = format!("{}.{}", m, r.join("."));
        let out = qualname_outcome(t, &full);
        if !matches!(out, Outcome::Unresolved) {
            return out;
        }
        if r.len() >= 2 {
            let class_qn = format!("{}.{}", m, r[..r.len() - 1].join("."));
            if class_exists(t, &class_qn) {
                return mro_outcome(t, &class_qn, r.last().unwrap());
            }
            // Re-exported class or submodule via `__init__.py`: `pkg.ReExportedCls.m()`.
            let mut seen: HashSet<(&str, &str)> = HashSet::new();
            if let Some(Outcome::Extracted(idx)) =
                follow_binding(t, (m.as_str(), r[0].as_str()), 0, &mut seen, &r[0])
            {
                if is_class_kind(t.symbols[idx].kind) {
                    let mut cur_class = t.symbols[idx].qualname.clone();
                    for field in &r[1..r.len() - 1] {
                        let Some(next) = resolve_self_attr_type(t, &cur_class, field) else {
                            return Outcome::Unresolved;
                        };
                        cur_class = next;
                    }
                    return mro_outcome(t, &cur_class, r.last().unwrap());
                }
                if t.symbols[idx].kind == SymbolKind::Module {
                    return chain_from_module(t, lang, t.symbols[idx].qualname.clone(), &r[1..]);
                }
            }
        }
        if is_stdlib_module(lang, &m) {
            return Outcome::Dropped;
        }
        return Outcome::Unresolved;
    }
    // Start module itself is external (import os; os.path.join(...)).
    if is_stdlib_module(lang, &module) {
        return Outcome::Dropped;
    }
    Outcome::Unresolved
}
