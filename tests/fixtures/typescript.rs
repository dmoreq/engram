//! TypeScript resolution fixtures: methods, constructors, imports, and namespaces.

use engram::extract::{extract_file, Confidence, EdgeKind};
use engram::parse::{parse_source, Lang};

#[test]
fn ts_class_method_and_this_call() {
    // R2: this.m() must resolve like Python self.m() — assert the EDGE.
    let src = "export class Ledger {\n  balance = 0\n  add(n: number): number {\n    return this.normalize(n)\n  }\n  private normalize(n: number): number {\n    return Math.abs(n)\n  }\n}\n";
    let parsed =
        engram::parse::parse_source(src.as_bytes(), engram::parse::Lang::TypeScript).unwrap();
    let fx = engram::extract::extract_file(0, "apps/core/ledger.ts", src.as_bytes(), &parsed);
    let names: Vec<&str> = fx.symbols.iter().map(|s| s.qualname.as_str()).collect();
    assert!(names.contains(&"apps.core.ledger.Ledger"), "{names:?}");
    assert!(names.contains(&"apps.core.ledger.Ledger.add"), "{names:?}");
    assert!(
        names.contains(&"apps.core.ledger.Ledger.normalize"),
        "{names:?}"
    );
    let resolution = engram::extract::resolve(vec![fx], Default::default());
    let hit = resolution.edges.iter().any(|e| {
        e.kind == EdgeKind::Calls
            && e.confidence == Confidence::Extracted
            && e.dst_name.contains("Ledger.normalize")
    });
    assert!(
        hit,
        "this.normalize(n) must resolve: {:?}",
        resolution.edges
    );
}

#[test]
fn ts_new_expression_resolves_to_constructor() {
    let src1 = "export class Widget {}\n";
    let src2 = "import { Widget } from './widget'\n\nexport function make(): Widget {\n  const w = new Widget()\n  return w\n}\n";
    let p1 = engram::parse::parse_source(src1.as_bytes(), engram::parse::Lang::TypeScript).unwrap();
    let p2 = engram::parse::parse_source(src2.as_bytes(), engram::parse::Lang::TypeScript).unwrap();
    let fx1 = engram::extract::extract_file(0, "apps/core/widget.ts", src1.as_bytes(), &p1);
    let fx2 = engram::extract::extract_file(1, "apps/core/factory.ts", src2.as_bytes(), &p2);
    let resolution = engram::extract::resolve(vec![fx1, fx2], Default::default());
    let hit = resolution.edges.iter().any(|e| {
        e.kind == EdgeKind::Calls
            && e.confidence == Confidence::Extracted
            && e.dst_name.contains("Widget")
    });
    assert!(hit, "new Widget() must resolve: {:?}", resolution.edges);
}

#[test]
fn ts_relative_import_resolves_by_path() {
    let src1 = "export function format(n: number): string {\n  return String(n)\n}\n";
    let src2 = "import { format } from '../lib/format'\n\nexport function run(n: number): string {\n  return format(n)\n}\n";
    let p1 = engram::parse::parse_source(src1.as_bytes(), engram::parse::Lang::TypeScript).unwrap();
    let p2 = engram::parse::parse_source(src2.as_bytes(), engram::parse::Lang::TypeScript).unwrap();
    let fx1 = engram::extract::extract_file(0, "apps/web/src/lib/format.ts", src1.as_bytes(), &p1);
    let fx2 =
        engram::extract::extract_file(1, "apps/web/src/features/run.ts", src2.as_bytes(), &p2);
    let resolution = engram::extract::resolve(vec![fx1, fx2], Default::default());
    let hit = resolution.edges.iter().any(|e| {
        e.kind == EdgeKind::Calls
            && e.confidence == Confidence::Extracted
            && e.dst_name.contains("format")
    });
    assert!(
        hit,
        "../lib/format import must resolve: {:?}",
        resolution.edges
    );
}

#[test]
fn ts_external_import_calls_drop() {
    let src = "import { useState } from 'react'\n\nexport function useCount() {\n  const [n] = useState(0)\n  return n\n}\n";
    let parsed =
        engram::parse::parse_source(src.as_bytes(), engram::parse::Lang::TypeScript).unwrap();
    let fx = engram::extract::extract_file(0, "apps/web/src/useCount.ts", src.as_bytes(), &parsed);
    let resolution = engram::extract::resolve(vec![fx], Default::default());
    assert!(
        !resolution
            .edges
            .iter()
            .any(|e| e.kind == EdgeKind::Calls && e.dst_name.contains("useState")),
        "no useState edge"
    );
    assert!(
        resolution.stats.builtin_dropped >= 1,
        "external call counted as dropped: {:?}",
        resolution.stats
    );
}

#[test]
fn typescript_namespaces_are_scopes() {
    let src = "namespace A { export function shared() {} }\nnamespace B { export function shared() {} }\n";
    let parsed = parse_source(src.as_bytes(), Lang::TypeScript).unwrap();
    let fx = extract_file(0, "pkg/mod.ts", src.as_bytes(), &parsed);
    let qns: Vec<&str> = fx.symbols.iter().map(|s| s.qualname.as_str()).collect();
    assert!(qns.iter().any(|q| q.ends_with(".A.shared")), "{qns:?}");
    assert!(qns.iter().any(|q| q.ends_with(".B.shared")), "{qns:?}");
}

#[test]
fn ts_object_literal_method_call_sites_resolve() {
    // Object-literal method shorthand is not a class member: the walker
    // descends into the body so its call sites resolve, and defines no
    // bogus method symbol for the shorthand name.
    let src = "function helper(): number {\n  return 1\n}\n\nconst handlers = {\n  run(): number {\n    return helper()\n  },\n}\n";
    let parsed = parse_source(src.as_bytes(), Lang::TypeScript).unwrap();
    let fx = extract_file(0, "pkg/handlers.ts", src.as_bytes(), &parsed);
    let names: Vec<&str> = fx.symbols.iter().map(|s| s.qualname.as_str()).collect();
    assert!(
        !names.iter().any(|n| n.ends_with(".handlers.run")),
        "object-literal shorthand must not define a method symbol: {names:?}"
    );
    let resolution = engram::extract::resolve(vec![fx], Default::default());
    assert!(
        resolution.edges.iter().any(|e| {
            e.kind == EdgeKind::Calls
                && e.confidence == Confidence::Extracted
                && e.dst_name.ends_with(".helper")
        }),
        "call inside the shorthand body must resolve: {:?}",
        resolution.edges
    );
}

#[test]
fn ts_export_default_class_binds_default_alias() {
    // `export default class Foo` binds the "default" alias like a
    // default-exported function does, so a consumer `import Repo from
    // './repo'` resolves `new Repo()` to the class.
    let src1 = "export default class Repo {}\n";
    let src2 = "import Repo from './repo'\n\nexport function pick(): Repo {\n  const r = new Repo()\n  return r\n}\n";
    let p1 = parse_source(src1.as_bytes(), Lang::TypeScript).unwrap();
    let p2 = parse_source(src2.as_bytes(), Lang::TypeScript).unwrap();
    let fx1 = extract_file(0, "pkg/repo.ts", src1.as_bytes(), &p1);
    let fx2 = extract_file(1, "pkg/pick.ts", src2.as_bytes(), &p2);
    let resolution = engram::extract::resolve(vec![fx1, fx2], Default::default());
    assert!(
        resolution.edges.iter().any(|e| {
            e.kind == EdgeKind::Calls
                && e.confidence == Confidence::Extracted
                && e.dst_name.ends_with(".Repo")
        }),
        "default-imported class constructor must resolve: {:?}",
        resolution.edges
    );
}

#[test]
fn ts_export_default_identifier_binds_alias() {
    // `export default makeRepo;` (identifier form) binds the "default" alias
    // to the referenced symbol, so a consumer's default import resolves the
    // call. A non-identifier default export stays unbound (negative below).
    let src1 = "export function makeRepo(): number {\n  return 1\n}\n\nexport default makeRepo;\n";
    let src2 =
        "import makeRepo from './repo'\n\nexport function run(): number {\n  return makeRepo()\n}\n";
    let p1 = parse_source(src1.as_bytes(), Lang::TypeScript).unwrap();
    let p2 = parse_source(src2.as_bytes(), Lang::TypeScript).unwrap();
    let fx1 = extract_file(0, "pkg/repo.ts", src1.as_bytes(), &p1);
    let fx2 = extract_file(1, "pkg/run.ts", src2.as_bytes(), &p2);
    let resolution = engram::extract::resolve(vec![fx1, fx2], Default::default());
    assert!(
        resolution.edges.iter().any(|e| {
            e.kind == EdgeKind::Calls
                && e.confidence == Confidence::Extracted
                && e.dst_name.ends_with(".makeRepo")
        }),
        "default-imported function call must resolve: {:?}",
        resolution.edges
    );
}

#[test]
fn ts_export_default_object_literal_stays_unresolved() {
    // `export default { ... }` has no identifier to alias: the import site
    // stays an unresolved call, never a fabricated edge.
    let src1 = "export default {\n  run(): number {\n    return 1\n  },\n};\n";
    let src2 = "import handler from './handler'\n\nexport function run(): number {\n  return handler.run()\n}\n";
    let p1 = parse_source(src1.as_bytes(), Lang::TypeScript).unwrap();
    let p2 = parse_source(src2.as_bytes(), Lang::TypeScript).unwrap();
    let fx1 = extract_file(0, "pkg/handler.ts", src1.as_bytes(), &p1);
    let fx2 = extract_file(1, "pkg/run.ts", src2.as_bytes(), &p2);
    let resolution = engram::extract::resolve(vec![fx1, fx2], Default::default());
    assert!(
        !resolution
            .edges
            .iter()
            .any(|e| { e.kind == EdgeKind::Calls && e.dst_name.ends_with(".handler") }),
        "object-literal default export must not fabricate an edge: {:?}",
        resolution.edges
    );
}

#[test]
fn ts_import_dot_slash_dot_dot_normalizes_directory() {
    // `./../shared` must pop the directory base even though the leading `./`
    // already consumed one segment: the resolved module is pkg.shared, not
    // pkg.sub.shared.
    let src1 = "export function make(n: number): number {\n  return n\n}\n";
    let src2 = "import { make } from './../shared'\n\nexport function run(): number {\n  return make(1)\n}\n";
    let p1 = parse_source(src1.as_bytes(), Lang::TypeScript).unwrap();
    let p2 = parse_source(src2.as_bytes(), Lang::TypeScript).unwrap();
    let fx1 = extract_file(0, "pkg/shared.ts", src1.as_bytes(), &p1);
    let fx2 = extract_file(1, "pkg/sub/run.ts", src2.as_bytes(), &p2);
    let resolution = engram::extract::resolve(vec![fx1, fx2], Default::default());
    let hit = resolution.edges.iter().any(|e| {
        e.kind == EdgeKind::Calls
            && e.confidence == Confidence::Extracted
            && e.dst_name == "pkg.shared.make"
    });
    assert!(
        hit,
        "./../shared must resolve to pkg.shared: {:?}",
        resolution.edges
    );
}
