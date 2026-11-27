//! Symbol/edge extraction and the call-target resolution algorithm (core IP).
//!
//! Two-phase design:
//! Phase A (per file, single tree walk): symbols (module/class/function/method),
//! inherits candidates, import bindings, typed parameters, constructor
//! assignments, and raw call sites.
//! Phase B (global): resolution over merged symbol tables — first match wins
//! per the spec order: local module -> import alias map -> receiver-class
//! method -> builtin table.
//!
//! Outcome: exactly one candidate -> EXTRACTED; multiple -> AMBIGUOUS
//! (dst_id NULL, dst_name carries the dotted callee); zero -> no edge, counted
//! unresolved. Dynamic dispatch (unknown receivers, getattr) is a non-goal.

use std::collections::{HashMap, HashSet};

use tree_sitter::Node;

use crate::parse::{Lang, ParsedFile};

mod paths;
pub use paths::module_path_for;
pub(crate) use paths::{is_test_name, is_test_path};
use paths::{rust_crate_root, rust_lib_root};

pub const SCHEMA_VERSION: i32 = 12;

#[derive(Debug, Clone, Default)]
pub struct NewSymbol {
    pub kind: SymbolKind,
    pub name: String,
    pub qualname: String,
    pub file_idx: usize,
    /// 1-based lines (tree-sitter rows are 0-based; offset at emission).
    pub line_start: usize,
    pub line_end: usize,
    /// One-line signature (Amendments 12a and 18): declaration header with
    /// line-wrapped parameters, whitespace collapsed, hard cap 160 chars.
    /// Empty for module/synthetic symbols. No body text.
    pub signature: String,
    /// Test-symbol flag (schema v8), classified from path, name, and test attrs.
    pub is_test: bool,
    /// Resolved return-type class qualname (schema v11), empty when absent.
    pub return_type: String,
}

#[derive(Debug, Clone)]
pub struct NewEdge {
    /// Index into the merged symbols vector.
    pub src_idx: usize,
    pub dst_idx: Option<usize>,
    pub dst_name: String,
    pub kind: EdgeKind,
    pub confidence: Confidence,
    pub site_line: usize,
}

#[derive(Debug, Default, Clone)]
pub struct ExtractStats {
    pub builtin_dropped: usize,
    pub unresolved_callees: usize,
}

/// One unresolved edge site (Amendment 19). `site_line` is 1-based in the
/// source file.
#[derive(Debug, Clone)]
pub struct UnresolvedSite {
    pub file_idx: usize,
    pub name: String,
    pub kind: EdgeSiteKind,
    pub site_line: usize,
}

#[derive(Debug, Clone)]
pub struct ResolvedClassField {
    pub file_idx: usize,
    pub class_qualname: String,
    pub field: String,
    pub target_qualname: String,
}

pub struct Resolution {
    pub symbols: Vec<NewSymbol>,
    /// Parallel to `symbols`: Some(row_id) for symbols carried forward from the
    /// previous index (already stored; not re-inserted), None for fresh rows.
    pub existing_ids: Vec<Option<i64>>,
    pub edges: Vec<NewEdge>,
    /// Resolved `self.field` / class-body field types for fresh files (schema v11).
    pub class_fields: Vec<ResolvedClassField>,
    pub module_exports: Vec<ResolvedModuleExport>,
    pub module_stars: Vec<ResolvedModuleStar>,
    /// Kept for the lib surface (integration tests assert builtin_dropped);
    /// the binary's meta total is the per-file SQL sum instead.
    #[allow(dead_code)]
    pub stats: ExtractStats,
    /// Unresolved edge sites as (fresh-file index, written name, kind, line).
    /// Amendment 15/19: lets the fast path re-extract exactly the carried
    /// files whose unresolved names became resolvable after a change.
    pub unresolved_calls: Vec<UnresolvedSite>,
    pub unresolved_per_file: Vec<usize>,
}

// ---------------------------------------------------------------------------
// Phase A: per-file extraction
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
enum ImportTarget {
    /// `import a.b.c` binds "a"; `import a.b.c as z` binds the full path to z.
    ModuleRoot { path: String },
    /// A module-level import edge target (Amendment 19): `import a.b`,
    /// `from pkg import mod`, TS `import './x'`, Rust `use`/`mod`.
    Module { path: String },
    /// `from m import n [as y]` binds the symbol (m, n).
    Symbol { module: String, name: String },
}

struct Assignment {
    var: String,
    target_dotted: Vec<String>,
    scope_func: String,
    /// 1-based line of the assignment; resolution uses the latest binding at
    /// or before each call site, not the file's final assignment.
    site_line: usize,
}

/// `self.field = Ctor(...)` inside a class body: receiver typing for
/// `self.field.m()` calls in any method of that class.
struct SelfField {
    class_qn: String,
    field: String,
    target_dotted: Vec<String>,
    site_line: usize,
}

struct ParamType {
    var: String,
    ann_dotted: Vec<String>,
    scope_func: String,
}

/// Parameter of an anonymous arrow/function expression, scoped by source line
/// range rather than a named symbol.
struct ArrowParam {
    var: String,
    scope_func: String,
    start_line: usize,
    end_line: usize,
}

struct CallSite {
    line: usize,
    scope_func: String,
    scope_class: Option<String>,
    /// Innermost inline `mod` qualname (Rust), for `self::`/`super::` paths.
    scope_mod: Option<String>,
    callee: Callee,
}

enum Callee {
    Name(String),
    Attr(Vec<String>),
}

struct ImportEdgeCandidate {
    line: usize,
    target: ImportTarget,
    maybe_external: bool,
}

impl ImportEdgeCandidate {
    fn module(line: usize, path: String) -> Self {
        Self {
            line,
            target: ImportTarget::Module { path },
            maybe_external: false,
        }
    }
    fn symbol(line: usize, module: String, name: String) -> Self {
        Self {
            line,
            target: ImportTarget::Symbol { module, name },
            maybe_external: false,
        }
    }
    fn module_root(line: usize, path: String) -> Self {
        Self {
            line,
            target: ImportTarget::ModuleRoot { path },
            maybe_external: false,
        }
    }
    fn with_maybe_external(mut self, maybe_external: bool) -> Self {
        self.maybe_external = maybe_external;
        self
    }
}

/// Binding records one file's walk collects for resolution: constructor-typed
/// assignments, parameter annotations, shadowing locals, aliases, `self.field`
/// targets, anonymous-callback parameters, star imports, and `from pkg import`
/// parent edges. Grouped so a new binding concept is declared here, initialized
/// by `FileBindings::default`, moved by one field (`bindings`), and folded in
/// `resolve::fold_binding_maps` — instead of per-field edits across `for_file`,
/// `merge_fresh`, and `FileCtx` (the extraction -> resolution hand-off debt).
#[derive(Default)]
struct FileBindings {
    assignments: Vec<Assignment>,
    params: Vec<ParamType>,
    /// (scope_func, var) for parameters and local bindings that shadow outer
    /// names during bare-name resolution.
    locals: Vec<(String, String)>,
    /// Anonymous callback parameters, scoped by line range.
    arrow_params: Vec<ArrowParam>,
    self_fields: Vec<SelfField>,
    /// (scope_func, bound name, target): function-local imports stay local.
    aliases: Vec<(String, String, ImportTarget)>,
    /// Modules appearing in `from m import *`.
    /// Star-imported modules with the statement line (per-statement scope).
    stars: Vec<(String, usize)>,
    /// Enclosing `alias_scope` for scoped Rust star imports (`(module, line) -> scope`).
    star_scopes: HashMap<(String, usize), String>,
    /// Rust star imports `(module, line)` that may be external crates (validated in Phase B).
    maybe_external_stars: HashSet<(String, usize)>,
    /// Rust aliases `(scope, bound_name)` that may be external crates (validated in Phase B).
    maybe_external_aliases: HashSet<(String, String)>,
    /// Python `from pkg import ...` parent module edges: (line, module).
    from_import_parents: HashSet<(usize, String)>,
    /// Python function/method/`@property` return type annotations: (qualname, chain).
    return_types: Vec<(String, Vec<String>)>,
}

pub struct FileExtraction {
    pub file_idx: usize,
    pub module: String,
    /// Rust crate root: a directory segment such as "src", or "" for a
    /// repo-root lib.rs/main.rs.
    pub crate_root: Option<String>,
    /// Crate root used for `use <crate_name>::...` (covers `tests/*.rs` -> `"src"`).
    pub lib_root: Option<String>,
    /// Normalized Cargo crate name (`[lib] name` or `[package] name` with `-` -> `_`).
    pub crate_name: Option<String>,
    /// Dot-joined DIRECTORY of the file (no file stem) — relative TS imports
    /// resolve against this, because stems may contain dots (x.test.ts).
    pub dir: String,
    pub is_package: bool,
    /// Test-file classification (schema v8); every symbol of this file
    /// inherits it (is_test_path).
    pub is_test_file: bool,
    pub lang: Lang,
    pub symbols: Vec<NewSymbol>,
    /// qualname -> first symbol index, maintained by [`Self::push_symbol`].
    /// Every parent lookup (`export_method`, Rust impl targets) is one map
    /// hit instead of a linear scan over the file's symbols.
    symbol_index: HashMap<String, usize>,
    /// (src local idx, base dotted chain)
    inherits: Vec<(usize, Vec<String>)>,
    /// Rust `impl Trait for Type` edges: edge-only, never `class_bases`.
    impl_inherits: Vec<(usize, Vec<String>, usize)>,
    /// Rust impls collected before symbol lookup:
    /// (target qualname, trait chain, impl line). Same-file only in v1.
    rust_impls: Vec<(String, Vec<String>, usize)>,
    /// Binding records folded into lookup maps during resolution (phase B);
    /// see [`FileBindings`] for the grouping rationale.
    bindings: FileBindings,
    /// Unresolvable inheritance sites: (base name, line).
    unresolved_inherits: Vec<(String, usize)>,
    calls: Vec<CallSite>,
    import_edges: Vec<ImportEdgeCandidate>,
}

impl FileExtraction {
    /// Empty extraction for one file: module path, language, crate root, and
    /// the module symbol. The caller walks the tree; the language comes from
    /// the caller, which resolved it explicitly against the walk allowlist.
    fn for_file(file_idx: usize, rel: &str, lang: Lang) -> Self {
        let (module, is_package) = module_path_for(rel);
        let (crate_root, lib_root) = if lang == Lang::Rust {
            (rust_crate_root(rel), rust_lib_root(rel))
        } else {
            (None, None)
        };
        let mod_name = module.rsplit('.').next().unwrap_or(&module).to_string();
        let is_test_file = is_test_path(rel);
        let dir = match rel.rfind('/') {
            Some(i) => rel[..i].replace('/', "."),
            None => String::new(),
        };
        FileExtraction {
            file_idx,
            module: module.clone(),
            crate_root,
            lib_root,
            crate_name: None,
            dir,
            is_package,
            is_test_file,
            lang,
            symbols: vec![NewSymbol {
                kind: SymbolKind::Module,
                name: mod_name,
                qualname: module.clone(),
                file_idx,
                line_start: 1,
                line_end: 1,
                signature: String::new(),

                is_test: is_test_file,
                return_type: String::new(),
            }],
            symbol_index: HashMap::from([(module, 0usize)]),
            inherits: Vec::new(),
            impl_inherits: Vec::new(),
            rust_impls: Vec::new(),
            bindings: FileBindings::default(),
            unresolved_inherits: Vec::new(),
            calls: Vec::new(),
            import_edges: Vec::new(),
        }
    }

    /// Push one symbol row and record its qualname in the lookup index.
    /// First occurrence wins, matching the linear `find`/`position` scans the
    /// parent lookups replaced.
    fn push_symbol(&mut self, sym: NewSymbol) {
        let idx = self.symbols.len();
        self.symbol_index.entry(sym.qualname.clone()).or_insert(idx);
        self.symbols.push(sym);
    }

    /// First same-file symbol index whose kind is Class and qualname is
    /// `target_qn`. The qualname index answers the common case in O(1); a
    /// qualname shared across kinds (broken source) falls back to the scan
    /// so a later Class still wins, matching the old linear search.
    fn class_symbol_idx(&self, target_qn: &str) -> Option<usize> {
        match self.symbol_index.get(target_qn) {
            Some(&idx) if self.symbols[idx].kind == SymbolKind::Class => Some(idx),
            Some(_) => self
                .symbols
                .iter()
                .position(|s| s.kind == SymbolKind::Class && s.qualname == target_qn),
            None => None,
        }
    }
}

/// One file's extraction inputs: merged-vector index, repo-relative path,
/// language, and source bytes — bundled so both entry points pass one
/// argument instead of four loose ones.
pub struct FileInput<'a> {
    pub file_idx: usize,
    pub rel: &'a str,
    pub lang: Lang,
    pub src: &'a [u8],
}

#[allow(dead_code)]
pub fn extract_file(file_idx: usize, rel: &str, src: &[u8], parsed: &ParsedFile) -> FileExtraction {
    extract_file_with_crate(file_idx, rel, src, parsed, None)
}

#[allow(dead_code)]
pub fn extract_file_with_crate(
    file_idx: usize,
    rel: &str,
    src: &[u8],
    parsed: &ParsedFile,
    crate_name: Option<&str>,
) -> FileExtraction {
    let root = parsed.tree.root_node();
    let lang = Lang::from_rel(rel).unwrap_or_else(|| panic!("unsupported source extension: {rel}"));
    extract_file_nodes_inner(
        FileInput {
            file_idx,
            rel,
            lang,
            src,
        },
        root,
        crate_name.map(str::to_string),
    )
}

/// Extraction from a parsed Tree-sitter root node.
pub fn extract_file_nodes(input: FileInput<'_>, root: Node<'_>) -> FileExtraction {
    let crate_name = (input.lang == Lang::Rust)
        .then(crate::walk::take_current_crate_name)
        .flatten();
    extract_file_nodes_inner(input, root, crate_name)
}

fn extract_file_nodes_inner(
    input: FileInput<'_>,
    root: Node<'_>,
    crate_name: Option<String>,
) -> FileExtraction {
    let mut fx = FileExtraction::for_file(input.file_idx, input.rel, input.lang);
    fx.crate_name = crate_name;
    fx.symbols[0].line_end = root.end_position().row + 1;
    let mut ctx = WalkCtx::for_file(fx.lang, input.src);
    walk_children(root, input.src, &mut fx, &mut ctx);
    finalize_rust_impls(&mut fx);
    fx
}

/// Resolve Rust `impl Trait for Type` pairs against same-file type symbols.
/// Cross-file impls are out of scope in v1 (Amendment 19).
fn finalize_rust_impls(fx: &mut FileExtraction) {
    let impls = std::mem::take(&mut fx.rust_impls);
    for (target_qn, trait_chain, line) in impls {
        if let Some(idx) = fx.class_symbol_idx(&target_qn) {
            fx.impl_inherits.push((idx, trait_chain, line));
        } else if let Some(base) = trait_chain.last() {
            fx.unresolved_inherits.push((base.clone(), line));
        }
    }
}

/// True when `qualname` is exactly `module.name` (no extra nesting). Avoids a
/// per-symbol `format!` allocation in the export walk.
pub(crate) fn is_direct_child(module: &str, name: &str, qualname: &str) -> bool {
    qualname.len() == module.len() + 1 + name.len()
        && qualname.starts_with(module)
        && qualname.as_bytes().get(module.len()) == Some(&b'.')
        && qualname.ends_with(name)
}

mod vocab;
use vocab::{is_builtin, is_stdlib_module};
pub use vocab::{
    CarriedIndex, CarriedInherits, CarriedSymbol, Confidence, EdgeKind, EdgeSiteKind,
    ResolvedModuleExport, ResolvedModuleStar, SymbolKind,
};

/// Per-language metric tables computed once per file instead of once per
/// callable (B2): the filtered decision kinds and the branch kinds.
mod walk;
pub use walk::split_ident;
use walk::{walk_children, WalkCtx};

mod resolve;
pub use resolve::resolve;
