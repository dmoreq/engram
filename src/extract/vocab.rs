//! Shared vocabularies for extraction and resolution: the schema-kind enums
//! and the builtin/stdlib name tables that gate the dropped-callee counters.

use crate::parse::Lang;

/// Python builtins (sorted for `binary_search`).
const PYTHON_BUILTINS: &[&str] = &[
    "ArithmeticError",
    "AssertionError",
    "AttributeError",
    "BaseException",
    "BlockingIOError",
    "BrokenPipeError",
    "BufferError",
    "ChildProcessError",
    "ConnectionError",
    "ConnectionRefusedError",
    "ConnectionResetError",
    "EOFError",
    "Exception",
    "FileExistsError",
    "FileNotFoundError",
    "FloatingPointError",
    "GeneratorExit",
    "ImportError",
    "IndentationError",
    "IndexError",
    "InterruptedError",
    "IsADirectoryError",
    "KeyError",
    "KeyboardInterrupt",
    "LookupError",
    "MemoryError",
    "ModuleNotFoundError",
    "NameError",
    "NotADirectoryError",
    "NotImplementedError",
    "OSError",
    "OverflowError",
    "PermissionError",
    "ProcessLookupError",
    "RecursionError",
    "ReferenceError",
    "RuntimeError",
    "StopAsyncIteration",
    "StopIteration",
    "SyntaxError",
    "SystemError",
    "SystemExit",
    "TabError",
    "TimeoutError",
    "TypeError",
    "UnboundLocalError",
    "UnicodeDecodeError",
    "UnicodeEncodeError",
    "UnicodeError",
    "UnicodeTranslateError",
    "ValueError",
    "ZeroDivisionError",
    "__import__",
    "abs",
    "aiter",
    "all",
    "anext",
    "any",
    "ascii",
    "bin",
    "bool",
    "bytearray",
    "bytes",
    "callable",
    "chr",
    "classmethod",
    "compile",
    "complex",
    "delattr",
    "dict",
    "dir",
    "divmod",
    "enumerate",
    "eval",
    "exec",
    "filter",
    "float",
    "format",
    "frozenset",
    "getattr",
    "globals",
    "hasattr",
    "hash",
    "hex",
    "id",
    "input",
    "int",
    "isinstance",
    "issubclass",
    "iter",
    "len",
    "list",
    "locals",
    "map",
    "max",
    "memoryview",
    "min",
    "next",
    "object",
    "oct",
    "open",
    "ord",
    "pow",
    "print",
    "property",
    "range",
    "repr",
    "reversed",
    "round",
    "set",
    "setattr",
    "slice",
    "sorted",
    "staticmethod",
    "str",
    "sum",
    "super",
    "tuple",
    "type",
    "vars",
    "zip",
];

/// Rust prelude items and stdlib crate roots (sorted for `binary_search`).
const RUST_PRELUDE: &[&str] = &[
    "Box", "Err", "Ok", "Option", "Result", "Some", "String", "Vec", "alloc", "core", "std",
];

/// TypeScript/JavaScript global builtins (sorted for `binary_search`).
const TS_GLOBALS: &[&str] = &[
    "Array",
    "Error",
    "JSON",
    "Map",
    "Math",
    "Object",
    "Promise",
    "Set",
    "console",
    "document",
    "fetch",
    "setTimeout",
    "window",
];

/// Python stdlib module roots (sorted for `binary_search`).
const PYTHON_STDLIB: &[&str] = &[
    "abc",
    "argparse",
    "array",
    "asyncio",
    "atexit",
    "base64",
    "bisect",
    "collections",
    "concurrent",
    "contextlib",
    "copy",
    "csv",
    "dataclasses",
    "datetime",
    "difflib",
    "enum",
    "errno",
    "fnmatch",
    "functools",
    "gc",
    "getpass",
    "glob",
    "gzip",
    "hashlib",
    "heapq",
    "http",
    "inspect",
    "io",
    "itertools",
    "json",
    "logging",
    "math",
    "multiprocessing",
    "operator",
    "os",
    "os.path",
    "pathlib",
    "pickle",
    "platform",
    "random",
    "re",
    "secrets",
    "shutil",
    "signal",
    "socket",
    "sqlite3",
    "stat",
    "statistics",
    "string",
    "struct",
    "subprocess",
    "sys",
    "tarfile",
    "tempfile",
    "textwrap",
    "threading",
    "time",
    "traceback",
    "types",
    "typing",
    "unicodedata",
    "unittest",
    "urllib",
    "uuid",
    "warnings",
    "weakref",
    "zipfile",
    "zlib",
];

const RUST_STDLIB: &[&str] = &["alloc", "core", "std"];

pub(super) fn is_builtin(lang: Lang, name: &str) -> bool {
    let table = match lang {
        Lang::Python => PYTHON_BUILTINS,
        Lang::Rust => RUST_PRELUDE,
        Lang::TypeScript | Lang::Tsx | Lang::JavaScript => TS_GLOBALS,
    };
    table.binary_search(&name).is_ok()
}

pub(super) fn is_stdlib_module(lang: Lang, module: &str) -> bool {
    let root = module.split('.').next().unwrap_or(module);
    match lang {
        Lang::Python => {
            PYTHON_STDLIB.binary_search(&module).is_ok()
                || PYTHON_STDLIB.binary_search(&root).is_ok()
        }
        Lang::Rust => RUST_STDLIB.binary_search(&root).is_ok(),
        Lang::TypeScript | Lang::Tsx | Lang::JavaScript => module.starts_with("node:"),
    }
}

/// Generate the shared lexical-vocabulary impl for one enum: the `VARIANTS`
/// table plus a `parse` that derives accepted spellings from `as_str` (defined
/// beside each enum), so a new variant cannot drift between the write and read
/// boundaries. One body replaces the per-enum copies; `as_str` stays per enum
/// because the spellings differ per vocabulary.
macro_rules! lexical_parse {
    ($name:ident { $($variant:ident),+ $(,)? }) => {
        impl $name {
            /// Every variant, in declaration order.
            const VARIANTS: &'static [$name] = &[$($name::$variant),+];

            /// Read boundary: `None` for foreign spellings, so carried rows
            /// with unknown spellings stay dropped.
            pub(crate) fn parse(kind: &str) -> Option<Self> {
                Self::VARIANTS
                    .iter()
                    .find(|variant| variant.as_str() == kind)
                    .copied()
            }
        }
    };
}

/// Symbol vocabulary stored in `symbols.kind` (schema contract). DB rows spell
/// kinds through `as_str()` at the bind boundary and parse back through
/// `parse` at the read boundary; in-memory comparisons use the variants so a
/// misspelled kind is a compile error instead of a silent wrong filter.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SymbolKind {
    #[default]
    Module,
    Class,
    Function,
    Method,
    Trait,
    Interface,
}

impl SymbolKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Module => "module",
            Self::Class => "class",
            Self::Function => "function",
            Self::Method => "method",
            Self::Trait => "trait",
            Self::Interface => "interface",
        }
    }
}

lexical_parse!(SymbolKind {
    Module,
    Class,
    Function,
    Method,
    Trait,
    Interface,
});

/// Edge vocabulary stored in `edges.kind`. No surface reads `contains` rows
/// (`db::edge_kinds_sql` omits it, matching the dead-code and gap-heal
/// semantics) and extraction no longer emits them, so the variant is gone;
/// legacy indexes may still carry the rows unread.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum EdgeKind {
    Calls,
    Imports,
    Inherits,
    TypeRef,
}

impl EdgeKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Calls => "calls",
            Self::Imports => "imports",
            Self::Inherits => "inherits",
            Self::TypeRef => "type_ref",
        }
    }
}

lexical_parse!(EdgeKind {
    Calls,
    Imports,
    Inherits,
    TypeRef,
});

/// Edge confidence vocabulary stored in `edges.confidence` (schema contract).
/// DB rows spell confidences through `as_str()` at the bind boundary; SQL
/// filters render the spelling from the variant, and in-memory comparisons
/// use the variants so a misspelled confidence is a compile error instead of
/// a silent wrong filter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confidence {
    Extracted,
    Ambiguous,
}

impl Confidence {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Extracted => "EXTRACTED",
            Self::Ambiguous => "AMBIGUOUS",
        }
    }
}

lexical_parse!(Confidence {
    Extracted,
    Ambiguous,
});

/// Unresolved-site vocabulary stored in `unresolved_calls.kind` (schema
/// contract). Same boundary discipline as `SymbolKind`/`EdgeKind`: rows spell
/// kinds through `as_str()` at the bind boundary and in-memory comparisons
/// use the variants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EdgeSiteKind {
    Call,
    Import,
    Inherit,
}

impl EdgeSiteKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Call => "call",
            Self::Import => "import",
            Self::Inherit => "inherit",
        }
    }
}

/// Symbols loaded from the previous index's fast-path files.
pub struct CarriedSymbol {
    pub row_id: i64,
    pub kind: SymbolKind,
    pub name: String,
    pub qualname: String,
    pub module: String,
    pub return_type: String,
}

/// Inheritance pairs read from the previous index (subclass qualname, base qualname).
pub type CarriedInherits = Vec<(String, String)>;

/// Previous-index state joined into resolution (fast path).
#[derive(Default)]
pub struct CarriedIndex {
    pub symbols: Vec<CarriedSymbol>,
    pub inherits: CarriedInherits,
    pub class_fields: Vec<(String, String, String)>,
    pub reexports: Vec<(String, String, i64)>,
    pub module_stars: Vec<(String, String)>,
}

#[derive(Debug, Clone)]
pub struct ResolvedModuleExport {
    pub file_idx: usize,
    pub module_qualname: String,
    pub exported_name: String,
    pub target_sym_idx: usize,
}

#[derive(Debug, Clone)]
pub struct ResolvedModuleStar {
    pub file_idx: usize,
    pub module_qualname: String,
    pub star_module: String,
}
