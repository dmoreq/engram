//! AST walk: symbol, call, import, and inheritance extraction for one file.
//!
//! The public entry (`extract_file`) lives in the parent; this module owns the
//! node dispatch and the per-file walking state.

use super::*;

mod bindings;
mod chain;
mod dispatch;
mod python;
mod rust;
mod rust_use;
mod typescript;

use bindings::*;
pub use chain::split_ident;
use chain::*;
pub(crate) use chain::{node_str, text};
pub(super) use dispatch::walk_children;
use dispatch::*;

pub(super) struct WalkCtx {
    class_stack: Vec<String>, // class/impl qualnames
    mod_stack: Vec<String>,   // inline `mod` qualnames (Rust)
    func_stack: Vec<String>,  // qualnames
    /// Test ancestry (schema v8): inside a Rust `#[test]`/`#[cfg(test)]` item
    /// or a TS `describe(...)`/`it(...)` callback. Save/restore per scope.
    test_scope: bool,
}

impl WalkCtx {
    pub(super) fn for_file(_lang: Lang, _src: &[u8]) -> WalkCtx {
        WalkCtx {
            class_stack: Vec::new(),
            mod_stack: Vec::new(),
            func_stack: Vec::new(),
            test_scope: false,
        }
    }

    pub(super) fn scope_func(&self) -> String {
        self.func_stack.last().cloned().unwrap_or_default()
    }

    pub(super) fn alias_scope<'a>(&'a self, fallback: &'a str) -> &'a str {
        if self.func_stack.is_empty() && self.class_stack.is_empty() && self.mod_stack.is_empty() {
            ""
        } else {
            self.enclosing_scope(fallback)
        }
    }

    pub(super) fn parent_container(&self) -> Option<&str> {
        self.class_stack
            .last()
            .or_else(|| self.mod_stack.last())
            .map(|s| s.as_str())
    }

    pub(super) fn enclosing_scope<'a>(&'a self, fallback: &'a str) -> &'a str {
        [
            self.func_stack.last(),
            self.class_stack.last(),
            self.mod_stack.last(),
        ]
        .into_iter()
        .flatten()
        .max_by_key(|qualname| qualname.len())
        .map(|s| s.as_str())
        .unwrap_or(fallback)
    }
}

/// Symbol fields shared by the callable constructors (function/method).
pub(super) struct CallableSpec<'a> {
    pub kind: SymbolKind,
    pub name: &'a str,
    pub qualname: &'a str,
    pub is_test: bool,
}

/// One node's mutable walk state, so the callable helpers need only a spec.
pub(super) struct Walker<'a, 's, 'b> {
    pub(super) node: Node<'a>,
    pub(super) src: &'s [u8],
    pub(super) fx: &'b mut FileExtraction,
    pub(super) ctx: &'b mut WalkCtx,
    pub(super) work: &'b mut Vec<Step<'a>>,
    pub(super) cursor: &'b mut tree_sitter::TreeCursor<'a>,
}

impl<'a, 's> Walker<'a, 's, '_> {
    /// Push the symbol row.
    pub(super) fn push_callable(&mut self, spec: &CallableSpec<'_>) {
        self.fx.push_symbol(NewSymbol::from_node(
            self,
            spec.kind,
            spec.name.to_string(),
            spec.qualname.to_string(),
            self.node,
            self.node,
            spec.is_test,
        ));
    }

    /// Record parameter types, then queue the body with its scope pop.
    pub(super) fn finish_callable(&mut self, spec: &CallableSpec<'_>) {
        if let Some(params) = self.node.child_by_field_name("parameters") {
            collect_param_types(self, params, spec.qualname, spec.kind == SymbolKind::Method);
        }
        if let Some(body) = self.node.child_by_field_name("body") {
            self.ctx.func_stack.push(spec.qualname.to_string());
            self.work.push(Step::PopFunc);
            push_children_rev(self.work, self.cursor, body);
        }
    }

    /// Shared CallableSpec constructor: the base is-test flag comes
    /// from the walked node.
    pub(super) fn callable_spec<'c>(
        &self,
        kind: SymbolKind,
        name: &'c str,
        qualname: &'c str,
    ) -> CallableSpec<'c> {
        CallableSpec {
            kind,
            name,
            qualname,
            is_test: is_test_item(self.fx, self.ctx, name),
        }
    }

    pub(super) fn push_local(&mut self, scope: &str, var: String) {
        self.fx.bindings.locals.push((scope.to_string(), var));
    }

    pub(super) fn push_return_type(&mut self, qualname: String, chain: Vec<String>) {
        self.fx.bindings.return_types.push((qualname, chain));
    }

    pub(super) fn push_alias(&mut self, scope: &str, bound: String, target: ImportTarget) {
        self.fx
            .bindings
            .aliases
            .push((scope.to_string(), bound, target));
    }
}

impl NewSymbol {
    /// Shared node-backed constructor: the one-line signature comes from
    /// `sig_node`, both 1-based line numbers from `line_node`, and the file
    /// index plus source bytes from the walk state, so the `push_symbol`
    /// emitters cannot drift apart field by field.
    pub(super) fn from_node(
        w: &Walker<'_, '_, '_>,
        kind: SymbolKind,
        name: String,
        qualname: String,
        line_node: Node<'_>,
        sig_node: Node<'_>,
        is_test: bool,
    ) -> Self {
        NewSymbol {
            kind,
            name,
            qualname,
            file_idx: w.fx.file_idx,
            signature: signature_of(sig_node, w.src),
            line_start: line_node.start_position().row + 1,
            line_end: line_node.end_position().row + 1,
            is_test,
            return_type: String::new(),
        }
    }
}
