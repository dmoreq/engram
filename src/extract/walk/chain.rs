//! Identifier, dotted-chain, signature, and type-unwrapping helpers for AST walking.

use super::*;

pub(crate) fn text(node: Node<'_>, src: &[u8]) -> String {
    node.utf8_text(src).unwrap_or("").to_string()
}

pub(crate) fn node_str<'a>(node: Node<'_>, src: &'a [u8]) -> &'a str {
    node.utf8_text(src).unwrap_or("")
}

/// One-line signature (Amendments 12a and 18): capture the declaration
/// header, continue across line wraps inside a parameter list, stop at the
/// body delimiter (`{`, `;`, or Python `:`), collapse whitespace, and cap
/// 160 chars. Never stores the body.
pub(super) fn signature_of(node: Node<'_>, src: &[u8]) -> String {
    let start = node.start_byte();
    let stop = src.len().min(start + 4096);
    let python = matches!(node.kind(), "function_definition" | "class_definition");
    let arrow = node.kind() == "arrow_function";
    let mut depth = 0i32;
    let mut end = start;
    let mut prev_non_space: u8 = 0;
    while end < stop {
        let b = src[end];
        // Comments never carry signature structure; skip them wholesale so a
        // decorated declaration or a brace inside a comment cannot truncate
        // the scan early.
        if b == b'/' && end + 1 < stop && src[end + 1] == b'/' {
            while end < stop && src[end] != b'\n' {
                end += 1;
            }
            continue;
        }
        if b == b'/' && end + 1 < stop && src[end + 1] == b'*' {
            end += 2;
            while end + 1 < stop && !(src[end] == b'*' && src[end + 1] == b'/') {
                end += 1;
            }
            end = (end + 2).min(stop);
            continue;
        }
        if b == b'#' && python {
            while end < stop && src[end] != b'\n' {
                end += 1;
            }
            continue;
        }
        match b {
            b'(' | b'[' => depth += 1,
            b')' | b']' => depth -= 1,
            b'{' => {
                if depth == 0 {
                    break;
                }
                depth += 1;
            }
            b'}' => depth -= 1,
            b';' if depth == 0 && !python => break,
            b':' if depth == 0 && python => break,
            // A brace-less TS arrow body (`() => expr`) must not be swallowed:
            // the signature ends with the `=>` marker itself.
            b'>' if arrow && prev_non_space == b'=' => break,
            b'\n'
                if depth == 0
                    && !matches!(
                        prev_non_space,
                        b',' | b'('
                            | b'['
                            | b'{'
                            | b'&'
                            | b'|'
                            | b'='
                            | b'>'
                            | b'<'
                            | b')'
                            | b'-'
                            | b':'
                    ) =>
            {
                break;
            }
            _ => {}
        }
        if !b.is_ascii_whitespace() {
            prev_non_space = b;
        }
        end += 1;
    }
    let raw = String::from_utf8_lossy(&src[start..end]);
    let mut collapsed = String::with_capacity(raw.len());
    for word in raw.split_whitespace() {
        if !collapsed.is_empty() {
            collapsed.push(' ');
        }
        collapsed.push_str(word);
    }
    collapsed.chars().take(160).collect()
}

/// Split an identifier into space-separated search tokens (Amendment 12).
/// Lowercases everything, splits camelCase/PascalCase transitions and runs of
/// underscores, collapses runs, and strips non-alphanumeric noise.
pub fn split_ident(ident: &str) -> String {
    let mut out = String::with_capacity(ident.len() + 4);
    let mut chars = ident.chars().peekable();
    let mut prev: Option<char> = None;
    while let Some(ch) = chars.next() {
        if ch == '_' || !ch.is_alphanumeric() {
            out.push(' ');
            prev = Some(ch);
            continue;
        }
        if ch.is_uppercase() {
            if let Some(prev_char) = prev {
                let next_lower = chars.peek().is_some_and(|c| c.is_lowercase());
                // Split lowerCase|UPPER and the last capital of an acronym
                // before a lowercase run ("HTTPServer" -> http server) (R7).
                if prev_char.is_lowercase()
                    || prev_char.is_numeric()
                    || (prev_char.is_uppercase() && next_lower)
                {
                    out.push(' ');
                }
            }
        }
        out.push(ch.to_ascii_lowercase());
        prev = Some(ch);
    }
    out
}

/// Dotted identifier chain for identifier/attribute nodes: `a.b.c` -> [a, b, c].
/// Unwraps `type` wrapper nodes (annotations). Rust path keywords (`crate`,
/// `self`, `super`) join the chain as plain segments.
pub(super) fn dotted_chain(node: Node<'_>, src: &[u8]) -> Option<Vec<String>> {
    dotted_chain_inner(node, src, false)
}

/// Dotted identifier chain for type-annotation positions: enables simple
/// dotted string forward references (`"PlaceRanker"`, `"pkg.PlaceRanker"`).
pub(super) fn type_dotted_chain(node: Node<'_>, src: &[u8]) -> Option<Vec<String>> {
    dotted_chain_inner(node, src, true)
}

fn dotted_chain_inner(node: Node<'_>, src: &[u8], allow_str: bool) -> Option<Vec<String>> {
    match node.kind() {
        // Leaf kinds. Rust receiver and field access: `self.foo()` / `x.foo()`;
        // `self` is its own node kind in the Rust grammar, and
        // `field_identifier` is the accessed member.
        "identifier" | "this" | "super" | "crate" | "type_identifier" | "self"
        | "field_identifier" => Some(vec![text(node, src)]),
        // Python `a.b` (field `attribute`), TS `a.b` (field `property`), and
        // Rust `a.b` (fields `value`/`field`) share the same chain rule.
        "attribute" | "member_expression" | "field_expression" => member_chain(node, src),
        // Rust scoped type paths in impl/trait positions: crate::Shape,
        // std::fmt::Display, and TS nested_type_identifier (ns.Base).
        "scoped_type_identifier" | "scoped_identifier" | "nested_type_identifier" => {
            scoped_chain(node, src)
        }
        // Annotation wrappers unwrap to the inner type's chain.
        "type" | "type_annotation" => unwrapped_chain(node, src),
        "generic_type" => generic_chain(node, src, allow_str),
        "subscript" => subscript_chain(node, src, allow_str),
        "union_type" | "binary_operator" => union_pipe_chain(node, src, allow_str),
        "string" if allow_str => forward_ref_chain(node, src),
        // Constructor-call receiver: `Base().m()` / `pkg.Cls().m()` /
        // `super().m()` / `super(C, self).m()` (Amendment 18). Non-uppercase
        // factory calls are left unresolved to avoid manufacturing class edges.
        "call" => call_chain(node, src),
        _ => None,
    }
}

/// The member-access arm: one chain from the receiver plus the attribute
/// text. The two field names differ per grammar.
fn member_chain(node: Node<'_>, src: &[u8]) -> Option<Vec<String>> {
    let (obj_field, attr_field) = match node.kind() {
        "field_expression" => ("value", "field"),
        "attribute" => ("object", "attribute"),
        _ => ("object", "property"),
    };
    let obj = node.child_by_field_name(obj_field)?;
    let attr = node.child_by_field_name(attr_field)?;
    let mut chain = dotted_chain(obj, src)?;
    chain.push(text(attr, src));
    Some(chain)
}

/// The annotation-wrapper arms: descend to the inner type. `type_annotation`
/// names its child with the `type` field; a plain `type` node has no such
/// field, so both fall back to the first named child.
fn unwrapped_chain(node: Node<'_>, src: &[u8]) -> Option<Vec<String>> {
    let inner = match node.child_by_field_name("type") {
        Some(ty) => Some(ty),
        None => node.named_child(0),
    };
    inner.and_then(|ch| dotted_chain_inner(ch, src, true))
}

/// The constructor-call arm: only `super`-led or uppercase-last receivers
/// produce a chain (handles both `Cls().m()` and `pkg.Cls().m()`).
fn call_chain(node: Node<'_>, src: &[u8]) -> Option<Vec<String>> {
    let func = node.child_by_field_name("function")?;
    let chain = dotted_chain(func, src)?;
    let first = chain.first()?;
    let last = chain.last()?;
    if first == "super" || last.chars().next().is_some_and(|c| c.is_uppercase()) {
        Some(chain)
    } else {
        None
    }
}

/// Unwrap Python `Optional[T]`, `Annotated[T, ...]`, `ClassVar[T]`, `Final[T]`,
/// single-type `Union[T, None]`, or `GenericClass[T]`. Falls back to Rust/TS
/// `generic_base` for `type_identifier` generics.
fn generic_chain(node: Node<'_>, src: &[u8], allow_str: bool) -> Option<Vec<String>> {
    if let (Some(base), Some(tp)) = (node.named_child(0), node.named_child(1)) {
        if matches!(base.kind(), "identifier" | "attribute") && tp.kind() == "type_parameter" {
            let base_chain = dotted_chain_inner(base, src, false)?;
            let mut cursor = tp.walk();
            let args: Vec<_> = tp.named_children(&mut cursor).collect();
            return unwrap_python_generic(base_chain, &args, src, allow_str);
        }
    }
    generic_base(node, src)
}

fn subscript_chain(node: Node<'_>, src: &[u8], allow_str: bool) -> Option<Vec<String>> {
    let value = node.child_by_field_name("value")?;
    let base_chain = dotted_chain_inner(value, src, false)?;
    let mut cursor = node.walk();
    let mut args: Vec<Node<'_>> = node
        .children_by_field_name("subscript", &mut cursor)
        .collect();
    if args.len() == 1 && matches!(args[0].kind(), "tuple" | "expression_list") {
        let mut inner_cursor = args[0].walk();
        args = args[0].named_children(&mut inner_cursor).collect();
    }
    unwrap_python_generic(base_chain, &args, src, allow_str)
}

fn unwrap_python_generic(
    base_chain: Vec<String>,
    args: &[Node<'_>],
    src: &[u8],
    allow_str: bool,
) -> Option<Vec<String>> {
    match base_chain.last().map(String::as_str) {
        Some("Optional" | "ClassVar" | "Final" | "Annotated") => args
            .first()
            .and_then(|&arg| dotted_chain_inner(arg, src, allow_str)),
        Some("Union") => {
            let non_none: Vec<_> = args
                .iter()
                .copied()
                .filter(|&a| !is_none_type_node(a, src))
                .collect();
            if non_none.len() == 1 {
                dotted_chain_inner(non_none[0], src, allow_str)
            } else {
                None
            }
        }
        _ => Some(base_chain),
    }
}

fn is_none_type_node(node: Node<'_>, src: &[u8]) -> bool {
    let inner = if node.kind() == "type" {
        node.named_child(0).unwrap_or(node)
    } else {
        node
    };
    inner.kind() == "none" || matches!(node_str(inner, src), "None" | "null" | "undefined")
}

fn union_pipe_chain(node: Node<'_>, src: &[u8], allow_str: bool) -> Option<Vec<String>> {
    let mut parts = Vec::new();
    collect_pipe_parts(node, src, &mut parts)?;
    let non_none: Vec<_> = parts
        .into_iter()
        .filter(|&p| !is_none_type_node(p, src))
        .collect();
    if non_none.len() == 1 {
        dotted_chain_inner(non_none[0], src, allow_str)
    } else {
        None
    }
}

fn collect_pipe_parts<'a>(node: Node<'a>, src: &[u8], out: &mut Vec<Node<'a>>) -> Option<()> {
    let inner = if node.kind() == "type" {
        node.named_child(0)?
    } else {
        node
    };
    match inner.kind() {
        "binary_operator" => {
            let op = inner.child_by_field_name("operator")?;
            if node_str(op, src) != "|" {
                return None;
            }
            let left = inner.child_by_field_name("left")?;
            let right = inner.child_by_field_name("right")?;
            collect_pipe_parts(left, src, out)?;
            collect_pipe_parts(right, src, out)?;
        }
        "union_type" => {
            let mut cursor = inner.walk();
            for child in inner.named_children(&mut cursor) {
                collect_pipe_parts(child, src, out)?;
            }
        }
        _ => out.push(inner),
    }
    Some(())
}

fn forward_ref_chain(node: Node<'_>, src: &[u8]) -> Option<Vec<String>> {
    let raw = node_str(node, src);
    let inner = raw
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .or_else(|| raw.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')))?;
    if inner.is_empty() || inner.starts_with('"') || inner.starts_with('\'') {
        return None;
    }
    let mut parts = Vec::new();
    for seg in inner.split('.') {
        let mut chars = seg.chars();
        let first = chars.next()?;
        if !(first.is_ascii_alphabetic() || first == '_')
            || !chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
        {
            return None;
        }
        parts.push(seg.to_string());
    }
    Some(parts)
}

/// Rust/TS scoped path (`a::b::c`, `ns.Base`): append `name` to the chain of `path`/`module`.
fn scoped_chain(node: Node<'_>, src: &[u8]) -> Option<Vec<String>> {
    let mut chain = node
        .child_by_field_name("path")
        .or_else(|| node.child_by_field_name("module"))
        .and_then(|p| dotted_chain(p, src))
        .unwrap_or_default();
    if let Some(name) = node.child_by_field_name("name") {
        chain.push(text(name, src));
    }
    (!chain.is_empty()).then_some(chain)
}

/// `Trait<T>`, `crate::pkg::Trait<T>`, `ns.Base<T>`: resolve the base type chain.
fn generic_base(node: Node<'_>, src: &[u8]) -> Option<Vec<String>> {
    if let Some(ty) = node
        .child_by_field_name("type")
        .or_else(|| node.child_by_field_name("name"))
    {
        if let Some(chain) = dotted_chain(ty, src) {
            return Some(chain);
        }
    }
    let mut stack = vec![node];
    while let Some(n) = stack.pop() {
        if n.kind() == "type_identifier" {
            return Some(vec![text(n, src)]);
        }
        let mut cursor = n.walk();
        let children: Vec<_> = n.children(&mut cursor).collect();
        for child in children.into_iter().rev() {
            stack.push(child);
        }
    }
    None
}

/// Strip `&`, `&mut`, and raw-pointer wrappers from a Rust type node.
pub(super) fn unwrap_ref_type(mut node: Node<'_>) -> Node<'_> {
    while matches!(node.kind(), "reference_type" | "pointer_type") {
        match node.child_by_field_name("type") {
            Some(inner) => node = inner,
            None => break,
        }
    }
    node
}

/// Binding name of a parameter node: identifier, named pattern, or typed name.
pub(super) fn param_binding(node: Node<'_>, src: &[u8]) -> Option<String> {
    node.child_by_field_name("name")
        .or_else(|| node.child_by_field_name("pattern"))
        .and_then(|n| pattern_name(n, src))
        .or_else(|| pattern_name(node, src))
}

/// Binding name of a declarator pattern: `x`, `mut x`, `(a, b)` (first name).
pub(super) fn pattern_name(node: Node<'_>, src: &[u8]) -> Option<String> {
    if node.kind() == "identifier" {
        return Some(text(node, src));
    }
    if node.kind() == "mut_pattern" {
        if let Some(inner) = node.child_by_field_name("pattern") {
            return pattern_name(inner, src);
        }
    }
    let mut cursor = node.walk();
    let ident = node
        .named_children(&mut cursor)
        .find(|c| c.kind() == "identifier");
    ident.map(|c| text(c, src))
}
