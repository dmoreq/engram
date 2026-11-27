//! Identifier tokenization and the stoplist that filters prelude/utility
//! callees and high-frequency shared names.

use super::corpus::SymbolData;
use super::*;
use std::collections::HashMap;

use crate::extract::split_ident;

/// Standard prelude/utility constructors and methods excluded from callee footprint.
/// Sorted for `binary_search`.
const STOPLIST: &[&str] = &[
    "Array",
    "Default::default",
    "Err",
    "None",
    "Object",
    "Ok",
    "Promise",
    "Promise.all",
    "Promise.resolve",
    "Some",
    "String::new",
    "Vec::new",
    "and_then",
    "any",
    "as_ref",
    "as_str",
    "bool",
    "boolean",
    "clone",
    "cls",
    "collect",
    "console",
    "contains",
    "def",
    "dict",
    "eprintln",
    "error",
    "float",
    "format",
    "from",
    "get",
    "insert",
    "int",
    "into",
    "into_iter",
    "is_empty",
    "isinstance",
    "iter",
    "len",
    "list",
    "log",
    "map",
    "new",
    "number",
    "open",
    "print",
    "println",
    "push",
    "range",
    "self",
    "set",
    "str",
    "string",
    "to_string",
    "tuple",
    "unwrap",
    "unwrap_or",
    "unwrap_or_default",
    "void",
    "warn",
];

/// Lowercase stoplist sorted for `binary_search` in `keep_token`.
const STOPLIST_LOWER: &[&str] = &[
    "and_then",
    "any",
    "array",
    "as_ref",
    "as_str",
    "bool",
    "boolean",
    "clone",
    "cls",
    "collect",
    "console",
    "contains",
    "def",
    "default::default",
    "dict",
    "eprintln",
    "err",
    "error",
    "float",
    "format",
    "from",
    "get",
    "insert",
    "int",
    "into",
    "into_iter",
    "is_empty",
    "isinstance",
    "iter",
    "len",
    "list",
    "log",
    "map",
    "new",
    "none",
    "number",
    "object",
    "ok",
    "open",
    "print",
    "println",
    "promise",
    "promise.all",
    "promise.resolve",
    "push",
    "range",
    "self",
    "set",
    "some",
    "str",
    "string",
    "string::new",
    "to_string",
    "tuple",
    "unwrap",
    "unwrap_or",
    "unwrap_or_default",
    "vec::new",
    "void",
    "warn",
];

pub(super) fn is_stoplisted(callee: &str) -> bool {
    STOPLIST.binary_search(&callee).is_ok()
}

pub(super) fn tokenize_name_sig(
    name: &str,
    sig: &str,
    include_all_tokens: bool,
) -> (HashSet<String>, HashSet<String>, HashSet<String>) {
    let mut all_tokens = HashSet::new();
    let mut type_tokens = HashSet::new();
    let mut name_tokens = HashSet::new();
    // One normalized insert shared by every scan: lowercase, keep the
    // three-plus-char non-stoplist tokens, and also file them into the
    // type-only set when they come from a typed position.
    let mut push_token = |tok: &str, typed: bool, from_name: bool| {
        let lower = tok.to_ascii_lowercase();
        if keep_token(&lower) {
            if typed {
                type_tokens.insert(lower.clone());
            }
            if from_name {
                if include_all_tokens {
                    name_tokens.insert(lower.clone());
                } else {
                    name_tokens.insert(lower);
                    return;
                }
            }
            if include_all_tokens {
                all_tokens.insert(lower);
            }
        }
    };

    for tok in split_ident(name).split_whitespace() {
        push_token(tok, false, true);
    }

    for part in sig.split(['(', ')', ',', '{', ';']) {
        // Parameter (`:`) and return (`->`) annotations share one loop.
        for (_, typed) in [part.split_once(':'), part.split_once("->")]
            .into_iter()
            .flatten()
        {
            for tok in split_ident(typed).split_whitespace() {
                push_token(tok, true, false);
            }
        }
    }

    if include_all_tokens {
        for tok in split_ident(sig).split_whitespace() {
            push_token(tok, false, false);
        }
    }

    (all_tokens, type_tokens, name_tokens)
}

/// Shared tokenizer guard: keep lowercase tokens of at least three characters
/// that the stoplist does not cover.
fn keep_token(lower: &str) -> bool {
    lower.len() >= 3 && STOPLIST_LOWER.binary_search(&lower).is_err()
}

/// Name tokens carried by more than 5% of the corpus are generic vocabulary,
/// not a distinctive interface signature. Pruning them keeps D2's posting
/// lists bounded on corpora with a shared parameter/name vocabulary.
pub(super) fn build_name_stoplist(symbols: &HashMap<i64, SymbolData>) -> HashSet<String> {
    let mut freq: HashMap<&str, usize> = HashMap::new();
    for symbol in symbols.values() {
        for token in &symbol.name_tokens {
            *freq.entry(token.as_str()).or_default() += 1;
        }
    }
    let threshold = (symbols.len() as f64 * 0.05).max(10.0) as usize;
    freq.into_iter()
        .filter(|(_, count)| *count > threshold)
        .map(|(token, _)| token.to_string())
        .collect()
}

pub(super) fn build_stoplist(symbols: &HashMap<i64, SymbolData>) -> HashSet<String> {
    let mut stopset: HashSet<String> = STOPLIST.iter().map(|&s| s.to_string()).collect();
    let mut freq: HashMap<&str, usize> = HashMap::new();
    let total = symbols.len();

    for sym in symbols.values() {
        for callee in &sym.all_callees {
            *freq.entry(callee.as_str()).or_default() += 1;
        }
    }

    let threshold = (total as f64 * 0.05).max(4.0) as usize;
    for (callee, count) in freq {
        if count >= threshold && !is_raw_client(callee) {
            stopset.insert(callee.to_string());
        }
    }
    stopset
}
