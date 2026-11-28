use engram::extract::{
    extract_file, extract_file_with_crate, resolve, Confidence, EdgeKind, SymbolKind,
};
use engram::parse::{parse_source, syntax_stats, Lang};

#[test]
fn rust_raw_pointer_syntax_has_zero_error_nodes() {
    let src = "fn demo(mut x: i32) { let _a = &raw const x; let _b = &raw mut x; }\n";
    let parsed = parse_source(src.as_bytes(), Lang::Rust).unwrap();
    let stats = syntax_stats(parsed.tree.root_node());
    assert_eq!(
        stats.error_nodes, 0,
        "tree-sitter-rust 0.24 must parse &raw cleanly"
    );
}

#[test]
fn rust_relative_use_and_external_crate_validation() {
    let sub = "pub fn ping() {}\npub fn helper() {}\n";
    let lib = "use sub::*;\nuse sub::helper as h;\nuse sub_late::{late};\nuse rusqlite::*;\nuse serde_json::from_str;\nmod sub;\nmod sub_late;\nmod inner {\n    use leaf::run;\n    pub mod leaf { pub fn run() {} }\n    pub fn go() { run(); }\n}\npub fn caller() { ping(); h(); late(); from_str(); }\n";
    let late = "pub fn late() {}\n";
    let fx1 = extract_file(
        0,
        "src/sub.rs",
        sub.as_bytes(),
        &parse_source(sub.as_bytes(), Lang::Rust).unwrap(),
    );
    let fx2 = extract_file(
        1,
        "src/sub_late.rs",
        late.as_bytes(),
        &parse_source(late.as_bytes(), Lang::Rust).unwrap(),
    );
    let fx3 = extract_file(
        2,
        "src/lib.rs",
        lib.as_bytes(),
        &parse_source(lib.as_bytes(), Lang::Rust).unwrap(),
    );
    let res = resolve(vec![fx1, fx2, fx3], Default::default());

    for target in [
        "src.sub.ping",
        "src.sub.helper",
        "src.sub_late.late",
        "src.inner.leaf.run",
    ] {
        assert!(
            res.edges.iter().any(|e| e.kind == EdgeKind::Calls
                && e.confidence == Confidence::Extracted
                && e.dst_name == target),
            "expected call to {target}: {:?}",
            res.edges
        );
    }
    assert!(
        !res.unresolved_calls
            .iter()
            .any(|u| u.name.contains("rusqlite") || u.name.contains("serde_json")),
        "external crates must not fabricate unresolved sites: {:?}",
        res.unresolved_calls
    );
    assert!(
        !res.module_stars
            .iter()
            .any(|s| s.star_module.contains("rusqlite")),
        "external star import must not appear in module_stars: {:?}",
        res.module_stars
    );
}

#[test]
fn module_non_shadowing_in_rust_and_python() {
    // Rust: `mod foo; pub use foo::foo;` + `mod bar;` (no fn `bar`)
    let r_foo = "pub fn foo() {}\n";
    let r_bar = "pub fn other() {}\n";
    let r_lib = "mod foo;\nmod bar;\npub use foo::foo;\npub fn run() { foo(); crate::foo(); crate::bar(); }\n";
    let r_res = resolve(
        vec![
            extract_file(
                0,
                "src/foo.rs",
                r_foo.as_bytes(),
                &parse_source(r_foo.as_bytes(), Lang::Rust).unwrap(),
            ),
            extract_file(
                1,
                "src/bar.rs",
                r_bar.as_bytes(),
                &parse_source(r_bar.as_bytes(), Lang::Rust).unwrap(),
            ),
            extract_file(
                2,
                "src/lib.rs",
                r_lib.as_bytes(),
                &parse_source(r_lib.as_bytes(), Lang::Rust).unwrap(),
            ),
        ],
        Default::default(),
    );
    assert!(
        !r_res.edges.iter().any(|e| e.kind == EdgeKind::Calls
            && e.dst_idx
                .is_some_and(|i| r_res.symbols[i].kind == SymbolKind::Module)),
        "calls must never target SymbolKind::Module: {:?}",
        r_res.edges
    );
    assert_eq!(
        r_res
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Calls && e.dst_name == "src.foo.foo")
            .count(),
        2,
        "both foo() and crate::foo() must resolve to src.foo.foo: {:?}",
        r_res.edges
    );

    // Python: `pkg/foo.py` + `pkg/__init__.py` (`from .foo import foo`) + `pkg/bar.py`
    let p_foo = "def foo():\n    pass\n";
    let p_bar = "def other():\n    pass\n";
    let p_init = "from .foo import foo\n";
    let p_app = "import pkg\npkg.foo()\npkg.bar()\n";
    let p_res = resolve(
        vec![
            extract_file(
                0,
                "pkg/foo.py",
                p_foo.as_bytes(),
                &parse_source(p_foo.as_bytes(), Lang::Python).unwrap(),
            ),
            extract_file(
                1,
                "pkg/bar.py",
                p_bar.as_bytes(),
                &parse_source(p_bar.as_bytes(), Lang::Python).unwrap(),
            ),
            extract_file(
                2,
                "pkg/__init__.py",
                p_init.as_bytes(),
                &parse_source(p_init.as_bytes(), Lang::Python).unwrap(),
            ),
            extract_file(
                3,
                "app.py",
                p_app.as_bytes(),
                &parse_source(p_app.as_bytes(), Lang::Python).unwrap(),
            ),
        ],
        Default::default(),
    );
    assert!(
        p_res
            .edges
            .iter()
            .any(|e| e.kind == EdgeKind::Calls && e.dst_name == "pkg.foo.foo"),
        "pkg.foo() must resolve through __init__.py re-export: {:?}",
        p_res.edges
    );
    assert!(
        !p_res.edges.iter().any(|e| e.kind == EdgeKind::Calls
            && e.dst_idx
                .is_some_and(|i| p_res.symbols[i].kind == SymbolKind::Module)),
        "Python calls must never target SymbolKind::Module: {:?}",
        p_res.edges
    );
}

#[test]
fn rust_own_crate_name_and_binary_only_crate_reexport() {
    let util = "pub fn ping() {}\n";
    let main_src = "mod util;\npub use util::ping;\nfn main() { ping(); }\n";
    let caller = "use crate::ping;\nfn go() { ping(); crate::ping(); }\n";
    let it = "use my_app::util::ping;\nfn test_it() { ping(); }\n";
    let res = resolve(
        vec![
            extract_file_with_crate(
                0,
                "src/util.rs",
                util.as_bytes(),
                &parse_source(util.as_bytes(), Lang::Rust).unwrap(),
                Some("my_app"),
            ),
            extract_file_with_crate(
                1,
                "src/main.rs",
                main_src.as_bytes(),
                &parse_source(main_src.as_bytes(), Lang::Rust).unwrap(),
                Some("my_app"),
            ),
            extract_file_with_crate(
                2,
                "src/caller.rs",
                caller.as_bytes(),
                &parse_source(caller.as_bytes(), Lang::Rust).unwrap(),
                Some("my_app"),
            ),
            extract_file_with_crate(
                3,
                "tests/it.rs",
                it.as_bytes(),
                &parse_source(it.as_bytes(), Lang::Rust).unwrap(),
                Some("my_app"),
            ),
        ],
        Default::default(),
    );
    let ping_calls = res
        .edges
        .iter()
        .filter(|e| e.kind == EdgeKind::Calls && e.dst_name == "src.util.ping")
        .count();
    assert_eq!(
        ping_calls, 4,
        "expected 4 calls to src.util.ping (main, caller x2, tests/it): {:?}",
        res.edges
    );
}

#[test]
fn rust_scoped_mod_tests_stars_no_self_loop_and_star_class_methods() {
    let types = "pub struct Svc;\nimpl Svc { pub fn run(&self) {} }\n";
    let lib = "mod types;\nuse types::*;\npub fn helper() {}\npub fn go(s: Svc) { s.run(); }\n#[cfg(test)]\nmod tests {\n    use super::*;\n    fn t() { helper(); }\n}\n";
    let res = resolve(
        vec![
            extract_file(
                0,
                "src/types.rs",
                types.as_bytes(),
                &parse_source(types.as_bytes(), Lang::Rust).unwrap(),
            ),
            extract_file(
                1,
                "src/lib.rs",
                lib.as_bytes(),
                &parse_source(lib.as_bytes(), Lang::Rust).unwrap(),
            ),
        ],
        Default::default(),
    );
    assert!(
        !res.module_stars
            .iter()
            .any(|s| s.module_qualname == s.star_module),
        "module_stars must not contain self-loops: {:?}",
        res.module_stars
    );
    assert!(
        res.edges
            .iter()
            .any(|e| e.kind == EdgeKind::Calls && e.dst_name == "src.types.Svc.run"),
        "star-imported struct method must resolve: {:?}",
        res.edges
    );
    assert!(
        res.edges
            .iter()
            .any(|e| e.kind == EdgeKind::Calls && e.dst_name == "src.helper"),
        "mod tests use super::* must resolve helper(): {:?}",
        res.edges
    );
}

#[test]
fn python_module_root_reexport_param_typeref_and_module_line_end() {
    let base = "class Base:\n    def ping(self):\n        pass\n";
    let init = "from .base import Base\n";
    let app = "import pkg\ndef go(x: pkg.Base, y: Unknown):\n    x.ping()\ndef only_ann(x: pkg.Base):\n    pass\n";
    let fx_app = extract_file(
        2,
        "app.py",
        app.as_bytes(),
        &parse_source(app.as_bytes(), Lang::Python).unwrap(),
    );
    assert!(
        fx_app.symbols[0].line_end >= 5,
        "module line_end must reflect file end, got {}",
        fx_app.symbols[0].line_end
    );

    let res = resolve(
        vec![
            extract_file(
                0,
                "pkg/base.py",
                base.as_bytes(),
                &parse_source(base.as_bytes(), Lang::Python).unwrap(),
            ),
            extract_file(
                1,
                "pkg/__init__.py",
                init.as_bytes(),
                &parse_source(init.as_bytes(), Lang::Python).unwrap(),
            ),
            fx_app,
        ],
        Default::default(),
    );
    assert!(
        res.edges
            .iter()
            .any(|e| e.kind == EdgeKind::Calls && e.dst_name == "pkg.base.Base.ping"),
        "import pkg; x: pkg.Base; x.ping() must follow re-export: {:?}",
        res.edges
    );
    let typerefs: Vec<_> = res
        .edges
        .iter()
        .filter(|e| e.kind == EdgeKind::TypeRef && e.dst_name == "pkg.base.Base")
        .collect();
    assert_eq!(
        typerefs.len(),
        2,
        "both go and only_ann must emit TypeRef to pkg.base.Base: {typerefs:?}"
    );
}

#[test]
fn ts_export_ns_local_alias_and_commonjs_require() {
    let sub = "export function ping() {}\nexport function helper() {}\n";
    let local_mod = "function internal() {}\nexport { internal as renamed };\n";
    let barrel = "export * as ns from './sub';\n";
    let app = "import { ns } from './barrel';\nimport { renamed } from './local_mod';\nconst m = require('./sub');\nconst { ping, helper: h } = require('./sub');\nns.ping();\nrenamed();\nm.ping();\nping();\nh();\n";
    let res = resolve(
        vec![
            extract_file(
                0,
                "sub.ts",
                sub.as_bytes(),
                &parse_source(sub.as_bytes(), Lang::TypeScript).unwrap(),
            ),
            extract_file(
                1,
                "local_mod.ts",
                local_mod.as_bytes(),
                &parse_source(local_mod.as_bytes(), Lang::TypeScript).unwrap(),
            ),
            extract_file(
                2,
                "barrel.ts",
                barrel.as_bytes(),
                &parse_source(barrel.as_bytes(), Lang::TypeScript).unwrap(),
            ),
            extract_file(
                3,
                "app.ts",
                app.as_bytes(),
                &parse_source(app.as_bytes(), Lang::TypeScript).unwrap(),
            ),
        ],
        Default::default(),
    );
    let ping_count = res
        .edges
        .iter()
        .filter(|e| e.kind == EdgeKind::Calls && e.dst_name == "sub.ping")
        .count();
    assert_eq!(
        ping_count, 3,
        "ns.ping(), m.ping(), and ping() must all resolve: {:?}",
        res.edges
    );
    assert!(
        res.edges
            .iter()
            .any(|e| e.kind == EdgeKind::Calls && e.dst_name == "sub.helper"),
        "const {{ helper: h }} = require('./sub'); h() must resolve: {:?}",
        res.edges
    );
    assert!(
        res.edges
            .iter()
            .any(|e| e.kind == EdgeKind::Calls && e.dst_name == "local_mod.internal"),
        "export {{ internal as renamed }} must resolve: {:?}",
        res.edges
    );
}
