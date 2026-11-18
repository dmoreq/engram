//! TypeScript/TSX/JavaScript extraction: classes, interfaces, methods,
//! arrow functions, exports, imports, and module resolution.

use super::*;

mod imports;

pub(super) use imports::{push_ts_module_edge, record_export_ts, record_import_ts};

pub(super) fn collect_heritage(
    node: Node<'_>,
    src: &[u8],
    fx: &mut FileExtraction,
    class_idx: usize,
) {
    // Only outer type nodes are bases. Descending into every identifier turns
    // `Base<Arg>` into a base `Arg` and `std::error::Error` into `std`/`error`.
    let mut stack = vec![node];
    while let Some(n) = stack.pop() {
        match n.kind() {
            "type" => {
                let mut c = n.walk();
                if let Some(inner) = n.named_children(&mut c).next() {
                    stack.push(inner);
                };
            }
            "class_heritage"
            | "extends_clause"
            | "implements_clause"
            | "extends_type_clause"
            | "trait_bounds"
            | "bounds"
            | "argument_list" => push_clause_children(n, &mut stack),
            "call_expression" => record_mixin_base(n, src, fx, class_idx),
            "generic_type"
            | "type_identifier"
            | "identifier"
            | "member_expression"
            | "nested_type_identifier"
            | "scoped_type_identifier"
            | "attribute"
            | "subscript" => record_leaf_base(n, src, fx, class_idx),
            _ => {}
        }
    }
}

/// The clause arms: push every named child back onto the heritage stack.
fn push_clause_children<'a>(n: Node<'a>, stack: &mut Vec<Node<'a>>) {
    let mut c = n.walk();
    for child in n.named_children(&mut c) {
        stack.push(child);
    }
}

/// The `call_expression` arm: `extends mixin(Base, Other)` records the mixin
/// arguments as bases; a non-resolvable mixin name stays unresolved.
fn record_mixin_base(n: Node<'_>, src: &[u8], fx: &mut FileExtraction, class_idx: usize) {
    if let Some(name) = mixin_bases(n, src, fx, class_idx) {
        if matches!(fx.lang, Lang::TypeScript | Lang::Tsx | Lang::JavaScript) {
            fx.unresolved_inherits
                .push((name, n.start_position().row + 1));
        }
    }
}

/// The leaf arms: the node IS a base name — resolve its dotted chain.
fn record_leaf_base(n: Node<'_>, src: &[u8], fx: &mut FileExtraction, class_idx: usize) {
    if let Some(chain) = dotted_chain(n, src) {
        fx.inherits.push((class_idx, chain));
    }
}

/// `extends mixin(Base, Other)`: only the first identifier argument. Returns
/// the fallback name when no identifier argument resolved.
fn mixin_bases(
    node: Node<'_>,
    src: &[u8],
    fx: &mut FileExtraction,
    class_idx: usize,
) -> Option<String> {
    if let Some(args) = node.child_by_field_name("arguments") {
        let mut cursor = args.walk();
        for arg in args.named_children(&mut cursor) {
            if matches!(
                arg.kind(),
                "identifier" | "type_identifier" | "member_expression" | "scoped_type_identifier"
            ) {
                if let Some(chain) = dotted_chain(arg, src) {
                    fx.inherits.push((class_idx, chain));
                }
                return None;
            }
        }
    }
    Some(
        node.child_by_field_name("function")
            .map(|f| text(f, src))
            .unwrap_or_else(|| text(node, src)),
    )
}

pub(super) fn define_method_ts<'a>(w: &mut Walker<'a, '_, '_>) {
    // method_definition: name field is a property_identifier inside a class body.
    let Some(name_node) = w.node.child_by_field_name("name") else {
        push_children_rev(w.work, w.node);
        return;
    };
    // Object-literal method shorthand `{ foo() { bar(); } }` is not a class
    // member: descend into the body so its call sites are traversed instead
    // of silently dropping them.
    if w.node.parent().map(|p| p.kind()) != Some("class_body") {
        push_children_rev(w.work, w.node);
        return;
    }
    let Some(class_qn) = w.ctx.class_stack.last().cloned() else {
        push_children_rev(w.work, w.node);
        return;
    };
    let name = text(name_node, w.src);
    let qualname = format!("{class_qn}.{name}");
    let spec = w.callable_spec(SymbolKind::Method, &name, &qualname);
    define_callable(w, &spec);
}

/// True when a TS declaration is the target of `export default`.
pub(super) fn ts_default_export_ancestor(node: Node<'_>) -> bool {
    let mut cur = node.parent();
    while let Some(n) = cur {
        if n.kind() == "export_statement" {
            let mut cursor = n.walk();
            return n.children(&mut cursor).any(|c| c.kind() == "default");
        }
        match n.kind() {
            "variable_declarator"
            | "lexical_declaration"
            | "variable_declaration"
            | "parenthesized_expression" => cur = n.parent(),
            _ => return false,
        }
    }
    false
}

/// Module-level `const X = () => {...}` / `= function() {...}` -> function
/// symbol (React components and hooks live here).
pub(super) fn record_ts_arrow_function<'a>(w: &mut Walker<'a, '_, '_>) -> bool {
    if !w.ctx.class_stack.is_empty() || !w.ctx.func_stack.is_empty() {
        return false; // nested functions are not indexed symbols
    }
    let Some(name_node) = w.node.child_by_field_name("name") else {
        return false;
    };
    let Some(value) = w.node.child_by_field_name("value") else {
        return false;
    };
    if !matches!(
        value.kind(),
        "arrow_function" | "function" | "function_expression"
    ) {
        return false;
    }
    let name = text(name_node, w.src);
    let qualname = format!("{}.{}", w.fx.module, name);
    w.fx.push_symbol(NewSymbol::from_node(
        w,
        SymbolKind::Function,
        name.clone(),
        qualname.clone(),
        w.node,
        value,
        is_test_item(w.fx, w.ctx, &name),
    ));
    w.ctx.func_stack.push(qualname.clone());
    collect_arrow_params(w, value, &qualname, None);
    w.work.push(Step::PopFunc);
    push_children_rev(w.work, value);
    true
}

/// TS `const value = new C()` receiver binding. A plain call
/// (`const value = factory()`) has no knowable constructed type and is ignored.
pub(super) fn record_ts_variable_binding(
    node: Node<'_>,
    src: &[u8],
    fx: &mut FileExtraction,
    ctx: &mut WalkCtx,
) {
    let Some(name) = node
        .child_by_field_name("name")
        .and_then(|n| pattern_name(n, src))
    else {
        return;
    };
    if let Some(ty) = node.child_by_field_name("type") {
        push_param_type(fx, name.clone(), dotted_chain(ty, src), &ctx.scope_func());
    }
    let Some(value) = node.child_by_field_name("value") else {
        return;
    };
    if value.kind() != "new_expression" {
        return;
    }
    let Some(chain) = value
        .child_by_field_name("constructor")
        .and_then(|c| dotted_chain(c, src))
    else {
        return;
    };
    push_assignment(fx, name, chain, ctx, node.start_position().row + 1);
}

/// TS/TSX/JSX static ESM imports. Relative sources resolve by module path;
/// bare/aliased sources bind as EXTERNAL (calls into them drop with the
/// builtin counter, not the unresolved counter).
pub(super) fn visit_ts_node<'a>(w: &mut Walker<'a, '_, '_>) -> bool {
    match w.node.kind() {
        "class_declaration" | "abstract_class_declaration" => {
            define_class_symbol(w, SymbolKind::Class);
        }
        "function_declaration" | "generator_function_declaration" => {
            define_function(w, SymbolKind::Function);
        }
        "interface_declaration" => define_class_symbol(w, SymbolKind::Interface),
        "method_definition" => define_method_ts(w),
        "variable_declarator" => {
            if !record_ts_arrow_function(w) {
                record_ts_variable_binding(w.node, w.src, w.fx, w.ctx);
                push_children_rev(w.work, w.node);
            }
        }
        "arrow_function" | "function_expression" => {
            let start = w.node.start_position().row + 1;
            let end = w.node.end_position().row + 1;
            let node = w.node;
            let scope = w.ctx.scope_func();
            collect_arrow_params(w, node, &scope, Some((start, end)));
            push_children_rev(w.work, w.node);
        }
        "new_expression" => {
            record_call(w.node, w.src, w.fx, w.ctx);
            push_children_rev(w.work, w.node);
        }
        "import_statement" => record_import_ts(w.node, w.src, w.fx),
        "export_statement" => {
            record_export_ts(w.node, w.src, w.fx);
            push_children_rev(w.work, w.node);
        }
        "internal_module" | "module" => visit_internal_module(w),
        "assignment_expression" => {
            record_reassignment(w.node, w.src, w.fx, w.ctx);
            push_children_rev(w.work, w.node);
        }
        _ => return false,
    }
    true
}

fn visit_internal_module<'a>(w: &mut Walker<'a, '_, '_>) {
    let name = w
        .node
        .child_by_field_name("name")
        .map(|n| text(n, w.src))
        .unwrap_or_default();
    let parent = w
        .ctx
        .class_stack
        .last()
        .map(|s| s.as_str())
        .unwrap_or(&w.fx.module);
    let qualname = format!("{parent}.{name}");
    let item_is_test = is_test_item(w.fx, w.ctx, &name);
    push_scope_symbol(w.fx, w.node, name, &qualname, item_is_test);
    w.ctx.class_stack.push(qualname);
    let mut cursor = w.node.walk();
    let children: Vec<_> = w.node.children(&mut cursor).collect();
    w.work.push(Step::PopClass);
    for child in children.into_iter().rev() {
        if child.child_by_field_name("body").is_some() || child.kind() == "statement_block" {
            push_children_rev(w.work, child);
        }
    }
}
