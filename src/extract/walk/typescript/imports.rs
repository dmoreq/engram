//! TS/JS import and export surface: import-clause bindings, barrel
//! re-export aliases, default-export binding, and module-path resolution.

use super::*;

pub(crate) fn record_export_ts(node: Node<'_>, src: &[u8], fx: &mut FileExtraction) {
    if let Some(source_node) = node.child_by_field_name("source") {
        let line = node.start_position().row + 1;
        let module = push_ts_module_edge(fx, src, source_node, line);
        // Barrel re-exports (`export { Base } from './types'` and `export * from './sub'`).
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if child.kind() == "*" || node_str(child, src) == "*" {
                fx.bindings.stars.push((module.clone(), line));
                fx.import_edges
                    .push(ImportEdgeCandidate::module_root(line, module.clone()));
                continue;
            }
            if child.kind() != "export_clause" {
                continue;
            }
            let mut clause = child.walk();
            for spec in child.children(&mut clause) {
                if spec.kind() != "export_specifier" {
                    continue;
                }
                let Some(name_node) = spec.child_by_field_name("name") else {
                    continue;
                };
                let exported = text(name_node, src);
                let bound = spec
                    .child_by_field_name("alias")
                    .map(|a| text(a, src))
                    .unwrap_or_else(|| exported.clone());
                fx.bindings.aliases.push((
                    String::new(),
                    bound,
                    ImportTarget::Symbol {
                        module: module.clone(),
                        name: exported,
                    },
                ));
            }
        }
        return;
    }
    // `export default foo;` (identifier form): bind the file-local name
    // "default" to the referenced symbol so `import X from './this'` in
    // another file resolves through the re-export chase. Declaration forms
    // (`export default class Foo {}`) define their own symbol and need no
    // extra binding.
    let mut cursor = node.walk();
    let has_default = node.children(&mut cursor).any(|c| c.kind() == "default");
    if !has_default {
        return;
    }
    let Some(value) = node.child_by_field_name("value").or_else(|| {
        node.children(&mut node.walk())
            .find(|c| c.kind() == "identifier")
    }) else {
        return;
    };
    if value.kind() != "identifier" {
        return;
    }
    fx.bindings.aliases.push((
        String::new(),
        "default".to_string(),
        ImportTarget::Symbol {
            module: fx.module.clone(),
            name: text(value, src),
        },
    ));
}

/// Push one module edge for a TS/JS import source node and return its path.
pub(crate) fn push_ts_module_edge(
    fx: &mut FileExtraction,
    src: &[u8],
    node: Node<'_>,
    line: usize,
) -> String {
    let raw = text(node, src);
    let source = raw.trim_matches(|c| c == '\'' || c == '"');
    let module = resolve_ts_module(source, &fx.dir);
    fx.import_edges
        .push(ImportEdgeCandidate::module(line, module.clone()));
    module
}

pub(crate) fn record_import_ts(node: Node<'_>, src: &[u8], fx: &mut FileExtraction) {
    let line = node.start_position().row + 1;
    let Some(source_node) = node.child_by_field_name("source") else {
        return;
    };
    let module = push_ts_module_edge(fx, src, source_node, line);
    let ctx = ImportCtx {
        module: &module,
        line,
    };
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "import_clause" {
            import_clause_bindings(child, src, ctx, fx);
        }
    }
}

#[derive(Clone, Copy)]
struct ImportCtx<'a> {
    module: &'a str,
    line: usize,
}

fn import_clause_bindings(
    clause: Node<'_>,
    src: &[u8],
    ctx: ImportCtx<'_>,
    fx: &mut FileExtraction,
) {
    let mut cursor = clause.walk();
    for part in clause.children(&mut cursor) {
        match part.kind() {
            "identifier" => default_import_binding(part, src, ctx, fx),
            "namespace_import" => namespace_import_binding(part, src, ctx, fx),
            "named_imports" => named_import_bindings(part, src, ctx, fx),
            _ => {}
        }
    }
}

/// default import: import X from "..."
fn default_import_binding(part: Node<'_>, src: &[u8], ctx: ImportCtx<'_>, fx: &mut FileExtraction) {
    let name = text(part, src);
    fx.bindings.aliases.push((
        String::new(),
        name,
        ImportTarget::Symbol {
            module: ctx.module.to_string(),
            name: "default".to_string(),
        },
    ));
}

/// * as ns — bind ns to the module itself
fn namespace_import_binding(
    part: Node<'_>,
    src: &[u8],
    ctx: ImportCtx<'_>,
    fx: &mut FileExtraction,
) {
    let mut cursor = part.walk();
    for child in part.children(&mut cursor) {
        if child.kind() == "identifier" {
            fx.bindings.aliases.push((
                String::new(),
                text(child, src),
                ImportTarget::ModuleRoot {
                    path: ctx.module.to_string(),
                },
            ));
        }
    }
}

fn named_import_bindings(part: Node<'_>, src: &[u8], ctx: ImportCtx<'_>, fx: &mut FileExtraction) {
    let mut cursor = part.walk();
    for spec in part.children(&mut cursor) {
        if spec.kind() != "import_specifier" {
            continue;
        }
        let Some(name_node) = spec.child_by_field_name("name") else {
            continue;
        };
        let name = text(name_node, src);
        let bound = spec
            .child_by_field_name("alias")
            .map(|a| text(a, src))
            .unwrap_or_else(|| name.clone());
        fx.bindings.aliases.push((
            String::new(),
            bound,
            ImportTarget::Symbol {
                module: ctx.module.to_string(),
                name: name.clone(),
            },
        ));
        fx.import_edges.push(ImportEdgeCandidate::symbol(
            ctx.line,
            ctx.module.to_string(),
            name,
        ));
    }
}

/// TS/JS source extensions in resolution order, derived from the master
/// extension table (`parse::SOURCE_EXTENSIONS`) so a new TS-family extension
/// needs no edit here. `.mjs`/`.cjs` import specifiers resolve to `.mts`/
/// `.cts` sources under the same stem.
fn ts_source_extensions() -> impl Iterator<Item = &'static str> {
    crate::parse::SOURCE_EXTENSIONS
        .iter()
        .filter(|(_, lang)| lang.is_ts_js())
        .map(|(ext, _)| *ext)
}

/// Resolve a TS import source to a module path, relative to the FILE'S
/// DIRECTORY (stems may contain dots, so the directory — not the module
/// string — is the only trustworthy base). Module identities collapse the
/// source extension and a trailing `index`, matching `module_path_for`, so
/// `./x`, `./x.ts`, `./x.mjs`, and `./x/index.ts` all resolve to the same
/// indexed module. Bare sources map to dotted external names.
pub(crate) fn resolve_ts_module(source: &str, dir: &str) -> String {
    let stripped = ts_source_extensions()
        .find_map(|ext| source.strip_suffix(ext))
        .unwrap_or(source);
    let mut parts: Vec<&str> = if stripped == "."
        || stripped == ".."
        || stripped.starts_with("./")
        || stripped.starts_with("../")
    {
        if dir.is_empty() {
            Vec::new()
        } else {
            dir.split('.').collect()
        }
    } else {
        Vec::new()
    };
    // One normalization pass: every `..` pops one directory segment (a `./..`
    // spelling must not strand the pop behind a skipped `.`), and `.` is
    // always dropped from the output.
    for seg in stripped.split('/').filter(|s| !s.is_empty()) {
        if seg == ".." {
            parts.pop();
            continue;
        }
        if seg != "." {
            parts.push(seg);
        }
    }
    if parts.last() == Some(&"index") {
        parts.pop();
    }
    if parts.is_empty() {
        // Root-level index.* is the repository root module.
        return "root".to_string();
    }
    parts.join(".")
}
