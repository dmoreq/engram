//! Parameter, binding, and assignment extraction helpers.

use super::*;

/// Record one assignment binding at `line`.
pub(super) fn push_assignment(
    fx: &mut FileExtraction,
    var: String,
    target_dotted: Vec<String>,
    ctx: &WalkCtx,
    line: usize,
) {
    fx.bindings.assignments.push(Assignment {
        var,
        target_dotted,
        scope_func: ctx.scope_func(),
        site_line: line,
    });
}

/// Parameter scan helpers take the walking [`Walker`] (source text plus
/// extraction sink, never varying within one subtree) and only the arguments
/// that actually vary: the scanned node, its scope, and a shape flag.
pub(super) fn collect_param_types(
    w: &mut Walker<'_, '_, '_>,
    params: Node<'_>,
    scope: &str,
    is_method_receiver: bool,
) {
    let mut cursor = params.walk();
    for p in params.children(&mut cursor) {
        push_param_local(w, p, scope, is_method_receiver);
        match w.fx.lang {
            Lang::Rust => push_rust_param(w, p, scope),
            Lang::TypeScript | Lang::Tsx | Lang::JavaScript => push_ts_param(w, p, scope),
            Lang::Python => push_typed_param(w, p, scope),
        }
    }
}

/// Binding-name locals. Only the method's own implicit receiver is skipped; an
/// explicit self/cls/this parameter of a nested function shadows the class
/// receiver.
fn push_param_local(
    w: &mut Walker<'_, '_, '_>,
    p: Node<'_>,
    scope: &str,
    is_method_receiver: bool,
) {
    let Some(var) = param_binding(p, w.src) else {
        return;
    };
    if (var == "self" || var == "cls" || var == "this") && is_method_receiver {
        return;
    }
    w.push_local(scope, var);
}

/// Rust: `x: Foo`, `x: &Foo`, `x: &mut Foo`. `self`/`&self` parameters
/// carry no type and are skipped.
fn push_rust_param(w: &mut Walker<'_, '_, '_>, p: Node<'_>, scope: &str) {
    if p.kind() != "parameter" {
        return;
    }
    let (Some(pattern), Some(ty)) = (
        p.child_by_field_name("pattern"),
        p.child_by_field_name("type"),
    ) else {
        return;
    };
    let Some(var) = pattern_name(pattern, w.src) else {
        return;
    };
    let Some(chain) = dotted_chain(unwrap_ref_type(ty), w.src) else {
        return;
    };
    push_param_type(w.fx, var, Some(chain), scope);
}

/// TS: required_parameter / optional_parameter carry pattern + type.
fn push_ts_param(w: &mut Walker<'_, '_, '_>, p: Node<'_>, scope: &str) {
    if !matches!(p.kind(), "required_parameter" | "optional_parameter") {
        return;
    }
    let Some(ty) = p.child_by_field_name("type") else {
        return;
    };
    let Some(ident) = p.child_by_field_name("pattern") else {
        return;
    };
    push_param_type(w.fx, text(ident, w.src), dotted_chain(ty, w.src), scope);
}

/// typed_parameter has NO 'name' field (positional identifier);
/// typed_default_parameter does. The annotation lives in the 'type' field,
/// wrapped in a 'type' node.
fn push_typed_param(w: &mut Walker<'_, '_, '_>, p: Node<'_>, scope: &str) {
    if !matches!(p.kind(), "typed_parameter" | "typed_default_parameter") {
        return;
    }
    let ident = p.child_by_field_name("name").or_else(|| {
        (0..p.child_count())
            .filter_map(|i| p.child(i))
            .find(|ch| ch.kind() == "identifier")
    });
    let ty = p.child_by_field_name("type");
    let (Some(ident), Some(ty)) = (ident, ty) else {
        return;
    };
    push_param_type(w.fx, text(ident, w.src), dotted_chain(ty, w.src), scope);
}

/// Record one typed parameter when its annotation resolves to a dotted chain.
pub(super) fn push_param_type(
    fx: &mut FileExtraction,
    var: String,
    chain: Option<Vec<String>>,
    scope: &str,
) {
    if let Some(ann_dotted) = chain {
        fx.bindings.params.push(ParamType {
            var,
            ann_dotted,
            scope_func: scope.to_string(),
        });
    }
}

/// Arrow/function-expression parameters. A named module-level arrow owns its
/// symbol scope (`range = None`); an anonymous nested callback is scoped by
/// its source line range so it does not shadow calls outside the callback.
pub(super) fn collect_arrow_params(
    w: &mut Walker<'_, '_, '_>,
    node: Node<'_>,
    scope: &str,
    range: Option<(usize, usize)>,
) {
    if let Some(params) = node.child_by_field_name("parameters") {
        if range.is_some() {
            let mut cursor = params.walk();
            for p in params.children(&mut cursor) {
                if let Some(var) = param_binding(p, w.src) {
                    push_arrow_binding(w.fx, scope, var, range);
                }
                if w.fx.lang.is_ts_js() {
                    push_ts_param(w, p, scope);
                }
            }
        } else {
            collect_param_types(w, params, scope, false);
        }
    }
    if let Some(param) = node.child_by_field_name("parameter") {
        if let Some(var) = param_binding(param, w.src) {
            push_arrow_binding(w.fx, scope, var, range);
        }
    }
}

fn push_arrow_binding(
    fx: &mut FileExtraction,
    scope: &str,
    var: String,
    range: Option<(usize, usize)>,
) {
    match range {
        Some((start_line, end_line)) => fx.bindings.arrow_params.push(ArrowParam {
            var,
            scope_func: scope.to_string(),
            start_line,
            end_line,
        }),
        None => fx.bindings.locals.push((scope.to_string(), var)),
    }
}

/// Constructor-target scan for receiver typing. CONSERVATIVE (R1 fix): only a
/// DIRECT call RHS types the receiver — `var = Ctor(...)`. For `a or B(...)`
/// the last top-level OR operand is accepted (the fallback-construction idiom);
/// nested calls like `obj = factory(A())` type NOTHING (the outer call's
/// return type is unknowable). Handles `var = ...` (function-local) and
/// `self.field = ...` (class-wide).
pub(super) fn record_assignment(w: &mut Walker<'_, '_, '_>) {
    let node = w.node;
    let src = w.src;
    let Some(left) = node.child_by_field_name("left") else {
        return;
    };
    // Python `x: T = v` / bare `x: T` (annotated_assignment): the annotation
    // types the variable even when no value is assigned yet, so later calls
    // on x resolve through its class. Class-body fields (`scope_func` empty
    // inside a class) record into `self_fields` instead of leaking into
    // module-level `params`.
    let scope_func = w.ctx.scope_func();
    let line = node.start_position().row + 1;
    if left.kind() == "identifier" {
        if let Some(ty) = node.child_by_field_name("type") {
            let chain = type_dotted_chain(ty, src);
            if scope_func.is_empty() {
                if let Some(class_qn) = w.ctx.class_stack.last().cloned() {
                    if let Some(target_dotted) = chain {
                        w.fx.bindings.self_fields.push(SelfField {
                            class_qn,
                            field: text(left, src),
                            target_dotted,
                            site_line: line,
                        });
                    }
                } else {
                    push_param_type(w.fx, text(left, src), chain, &scope_func);
                }
            } else {
                push_param_type(w.fx, text(left, src), chain, &scope_func);
            }
        }
    } else if left.kind() == "attribute" {
        if let Some(ty) = node.child_by_field_name("type") {
            if let Some(target_dotted) = type_dotted_chain(ty, src) {
                record_self_field_assignment(left, target_dotted, w.ctx, w.fx, src);
            }
        }
    }
    if matches!(
        left.kind(),
        "pattern_list" | "tuple_pattern" | "list_pattern" | "expression_list" | "list"
    ) {
        record_pattern_shadow(left, src, w.fx, w.ctx, line);
        return;
    }
    let Some(right) = node.child_by_field_name("right") else {
        return;
    };
    let target = direct_call_target(right, src).or_else(|| {
        if right.kind() == "identifier" {
            same_scope_var_chain(&w.fx.bindings, &scope_func, node_str(right, src), line)
        } else {
            None
        }
    });
    let Some(target) = target else {
        // A simple `x = <unknown>` still overwrites an earlier constructor
        // type; record it as an unknown binding instead of leaving the old
        // assignment in place. An annotated assignment (`x: T = unknown`)
        // keeps its annotation instead: the empty binding would shadow the
        // type recorded above.
        if left.kind() == "identifier" && node.child_by_field_name("type").is_none() {
            push_assignment(w.fx, text(left, src), Vec::new(), w.ctx, line);
        }
        return;
    };
    match left.kind() {
        "identifier" => {
            // An annotated assignment's declared type outranks whatever the
            // RHS call suggests: the annotation is already recorded above,
            // and a plain-call target (build()) must not shadow it.
            if node.child_by_field_name("type").is_none() {
                push_assignment(w.fx, text(left, src), target, w.ctx, line);
            }
        }
        "attribute" if node.child_by_field_name("type").is_none() => {
            record_self_field_assignment(left, target, w.ctx, w.fx, src);
        }
        _ => {}
    }
}

/// Look up a variable's type chain from prior same-scope assignments or
/// parameter annotations (constructor injection `self.r = r`, local alias `y = x`).
fn same_scope_var_chain(
    bindings: &FileBindings,
    scope: &str,
    var: &str,
    line: usize,
) -> Option<Vec<String>> {
    if let Some(a) = bindings
        .assignments
        .iter()
        .rev()
        .find(|a| a.scope_func == scope && a.var == var && a.site_line <= line)
    {
        return (!a.target_dotted.is_empty()).then(|| a.target_dotted.clone());
    }
    bindings
        .params
        .iter()
        .rev()
        .find(|p| p.scope_func == scope && p.var == var)
        .map(|p| p.ann_dotted.clone())
}

/// `self.field = Ctor(...)` -> class-wide receiver typing.
fn record_self_field_assignment(
    left: Node<'_>,
    target: Vec<String>,
    ctx: &WalkCtx,
    fx: &mut FileExtraction,
    src: &[u8],
) {
    let Some(field) = left
        .child_by_field_name("object")
        .zip(left.child_by_field_name("attribute"))
        .and_then(|(obj, attr)| self_field_target(obj, attr, src))
    else {
        return;
    };
    let Some(class_qn) = ctx.class_stack.last().cloned() else {
        return;
    };
    fx.bindings.self_fields.push(SelfField {
        class_qn,
        field,
        target_dotted: target,
        site_line: left.start_position().row + 1,
    });
}

/// `x = Ctor(...)` / `x = a or Ctor(...)`: the constructor chain, if any.
fn direct_call_target(right: Node<'_>, src: &[u8]) -> Option<Vec<String>> {
    let direct = |call: Node<'_>| -> Option<Vec<String>> {
        let f = call
            .child_by_field_name("function")
            .or_else(|| call.child_by_field_name("constructor"))?;
        dotted_chain(f, src)
    };
    if matches!(right.kind(), "call" | "call_expression" | "new_expression") {
        direct(right)
    } else if matches!(right.kind(), "boolean_operator" | "bool_op") {
        // `x = a or B(...)`: the last top-level operand that is a direct call.
        let mut cursor = right.walk();
        right
            .named_children(&mut cursor)
            .filter(|o| matches!(o.kind(), "call" | "call_expression"))
            .last()
            .and_then(direct)
    } else {
        None
    }
}

/// The field name when the assignment target is `self.<field>`.
fn self_field_target(obj: Node<'_>, attr: Node<'_>, src: &[u8]) -> Option<String> {
    (obj.kind() == "identifier" && node_str(obj, src) == "self").then(|| text(attr, src))
}

/// TS/Rust `value = ...` reassignment. A direct constructor updates the
/// receiver type; an unknown right-hand side records an empty binding so the
/// later call cannot reuse an earlier type.
/// Tuple/list destructuring binds every identifier as an unknown local. The
/// binding shadows outer function names from its site line onward.
fn record_pattern_shadow(
    node: Node<'_>,
    src: &[u8],
    fx: &mut FileExtraction,
    ctx: &WalkCtx,
    site_line: usize,
) {
    // Heap stack instead of recursion: deeply nested destructuring (>500
    // levels) would otherwise overflow the thread stack.
    let mut stack = vec![node];
    while let Some(current) = stack.pop() {
        if current.kind() == "identifier" {
            push_assignment(fx, text(current, src), Vec::new(), ctx, site_line);
            continue;
        }
        // Attribute assignment targets (`self.x = v`, `obj.x = v`) are not
        // patterns: descending would record `x` as a shadowing local and erase
        // the receiver's known type for later calls.
        if matches!(
            current.kind(),
            "attribute" | "member_expression" | "field_expression"
        ) {
            continue;
        }
        let mut cursor = current.walk();
        for child in current.named_children(&mut cursor) {
            stack.push(child);
        }
    }
}

pub(super) fn record_reassignment(
    node: Node<'_>,
    src: &[u8],
    fx: &mut FileExtraction,
    ctx: &mut WalkCtx,
) {
    let Some(left) = node.child_by_field_name("left") else {
        return;
    };
    let Some(right) = node.child_by_field_name("right") else {
        return;
    };
    let target = match fx.lang {
        Lang::TypeScript | Lang::Tsx | Lang::JavaScript => direct_call_target(right, src),
        Lang::Rust => rust::rust_constructor_target(right, src, ctx),
        _ => None,
    };
    if left.kind() == "member_expression" {
        if let (Some(obj), Some(prop), Some(class_qn), Some(target_dotted)) = (
            left.child_by_field_name("object"),
            left.child_by_field_name("property"),
            ctx.class_stack.last().cloned(),
            target,
        ) {
            if obj.kind() == "this" {
                fx.bindings.self_fields.push(SelfField {
                    class_qn,
                    field: text(prop, src),
                    target_dotted,
                    site_line: left.start_position().row + 1,
                });
            }
        }
        return;
    }
    if left.kind() != "identifier" {
        return;
    }
    push_assignment(
        fx,
        text(left, src),
        target.unwrap_or_default(),
        ctx,
        node.start_position().row + 1,
    );
}
