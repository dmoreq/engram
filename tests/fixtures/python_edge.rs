//! Python annotation and inherit-edge fixtures: annotated-assignment
//! receiver typing, untyped-receiver negatives, and the AMBIGUOUS base edge.

use engram::extract::{
    extract_file, resolve, CarriedIndex, CarriedSymbol, Confidence, EdgeKind, EdgeSiteKind,
    SymbolKind,
};
use engram::parse::{parse_source, Lang};

#[test]
fn python_annotated_assignment_types_receiver() {
    // `x: Config = build()` (and a bare `y: Config`) must type the variable
    // through its annotation, so later calls on it resolve to the class.
    let src1 = "class Config:\n    def run(self):\n        return 1\n";
    let src2 = "from .config import Config\n\ndef build():\n    return None\n\ndef load():\n    x: Config = build()\n    y: Config\n    return x.run()\n";
    let p1 = parse_source(src1.as_bytes(), Lang::Python).unwrap();
    let p2 = parse_source(src2.as_bytes(), Lang::Python).unwrap();
    let fx1 = extract_file(0, "pkg/config.py", src1.as_bytes(), &p1);
    let fx2 = extract_file(1, "pkg/load.py", src2.as_bytes(), &p2);
    let resolution = resolve(vec![fx1, fx2], Default::default());
    let e = resolution
        .edges
        .iter()
        .map(|e| (e.kind, e.confidence, e.dst_name.as_str()))
        .collect::<Vec<_>>();
    assert!(
        e.iter().any(|(k, c, n)| {
            *k == EdgeKind::Calls && *c == Confidence::Extracted && n.ends_with(".Config.run")
        }),
        "annotated receiver call must resolve: {e:?}"
    );
}

#[test]
fn python_untyped_receiver_call_stays_unresolved() {
    // Without an annotation or constructor assignment the receiver call is a
    // genuine gap: it must land in unresolved_calls, not silently drop.
    let src1 = "class Config:\n    def run(self):\n        return 1\n";
    let src2 = "def load(raw):\n    return raw.run()\n";
    let p1 = parse_source(src1.as_bytes(), Lang::Python).unwrap();
    let p2 = parse_source(src2.as_bytes(), Lang::Python).unwrap();
    let fx1 = extract_file(0, "pkg/config.py", src1.as_bytes(), &p1);
    let fx2 = extract_file(1, "pkg/load.py", src2.as_bytes(), &p2);
    let resolution = resolve(vec![fx1, fx2], Default::default());
    assert!(
        resolution
            .unresolved_calls
            .iter()
            .any(|u| u.name == "raw.run"),
        "untyped receiver call must stay unresolved: {:?}",
        resolution.unresolved_calls
    );
}

#[test]
fn python_ambiguous_inherit_keeps_dst_less_edge() {
    // Two same-qualname bases across files: the child's base chain is
    // AMBIGUOUS. The edge stays visible with dst None and AMBIGUOUS
    // confidence, and the site still lands in unresolved_calls.
    // `pkg/sub.py` and `pkg.sub.py` both map to module pkg.sub, so the two
    // Base classes share one qualname — the only shape that produces a
    // multi-match candidate.
    let src1 = "class Base:\n    pass\n";
    let src2 = "class Base:\n    pass\n";
    let src3 = "class Impl(Base):\n    pass\n";
    let p1 = parse_source(src1.as_bytes(), Lang::Python).unwrap();
    let p2 = parse_source(src2.as_bytes(), Lang::Python).unwrap();
    let p3 = parse_source(src3.as_bytes(), Lang::Python).unwrap();
    let fx1 = extract_file(0, "pkg/sub.py", src1.as_bytes(), &p1);
    let fx2 = extract_file(1, "pkg.sub.py", src2.as_bytes(), &p2);
    let fx3 = extract_file(2, "pkg/sub/impl.py", src3.as_bytes(), &p3);
    let resolution = resolve(vec![fx1, fx2, fx3], Default::default());
    let e = resolution
        .edges
        .iter()
        .map(|e| (e.kind, e.confidence, e.dst_idx, e.dst_name.as_str()))
        .collect::<Vec<_>>();
    assert!(
        e.iter().any(|(k, c, d, n)| {
            *k == EdgeKind::Inherits
                && *c == Confidence::Ambiguous
                && d.is_none()
                && *n == "pkg.sub.Base"
        }),
        "ambiguous base must keep a dst-less AMBIGUOUS edge: {e:?}"
    );
    assert!(
        resolution
            .unresolved_calls
            .iter()
            .any(|u| u.name == "Base" && u.kind == EdgeSiteKind::Inherit),
        "ambiguous base site must surface in unresolved_calls: {:?}",
        resolution.unresolved_calls
    );
}

#[test]
fn tier1_type_unwrapping_and_union_negatives() {
    let pkg_src = "class PlaceRanker:\n    def rank(self):\n        return 1\n\nclass GenericRepo:\n    def fetch(self):\n        return 2\n";
    let app_src = r#"from typing import Optional, Annotated, ClassVar, Final, Union
import pkg
from pkg import PlaceRanker, GenericRepo

class Svc:
    cv: ClassVar[PlaceRanker]
    fin: Final["pkg.PlaceRanker"]

    def run(
        self,
        a: Optional[PlaceRanker],
        b: Annotated[PlaceRanker, "meta"],
        c: Union[PlaceRanker, None],
        d: Union[None, PlaceRanker],
        e: PlaceRanker | None,
        f: None | "PlaceRanker",
        g: pkg.GenericRepo[PlaceRanker],
        amb_u: Union[PlaceRanker, GenericRepo],
        amb_p: PlaceRanker | GenericRepo,
    ):
        a.rank()
        b.rank()
        c.rank()
        d.rank()
        e.rank()
        f.rank()
        self.cv.rank()
        self.fin.rank()
        g.fetch()
        pkg.PlaceRanker().rank()
        amb_u.rank()
        amb_p.rank()
"#;
    let p1 = parse_source(pkg_src.as_bytes(), Lang::Python).unwrap();
    let p2 = parse_source(app_src.as_bytes(), Lang::Python).unwrap();
    let fx1 = extract_file(0, "pkg.py", pkg_src.as_bytes(), &p1);
    let fx2 = extract_file(1, "app.py", app_src.as_bytes(), &p2);
    let res = resolve(vec![fx1, fx2], Default::default());
    let rank_lines: Vec<usize> = res
        .edges
        .iter()
        .filter(|e| {
            e.kind == EdgeKind::Calls
                && e.confidence == Confidence::Extracted
                && e.dst_name == "pkg.PlaceRanker.rank"
        })
        .map(|e| e.site_line)
        .collect();
    // Lines 21..28 are a..f + self.cv + self.fin; line 30 is pkg.PlaceRanker().rank().
    assert_eq!(
        rank_lines,
        vec![21, 22, 23, 24, 25, 26, 27, 28, 30],
        "expected unwrapped rank() calls, got {rank_lines:?}"
    );
    assert!(
        res.edges.iter().any(|e| e.kind == EdgeKind::Calls
            && e.confidence == Confidence::Extracted
            && e.dst_name == "pkg.GenericRepo.fetch"),
        "GenericRepo[T] must resolve fetch()"
    );
    // Multi-type unions amb_u (line 31) and amb_p (line 32) must remain unresolved.
    let unres_lines: Vec<usize> = res
        .unresolved_calls
        .iter()
        .filter(|u| u.name.ends_with(".rank"))
        .map(|u| u.site_line)
        .collect();
    assert_eq!(
        unres_lines,
        vec![31, 32],
        "multi-class Union/pipe must stay unresolved: {unres_lines:?}"
    );
}

#[test]
fn tier1_bindings_and_external_module_root_drop() {
    let src = r#"import polars as pl
import pytest

class Ranker:
    def rank(self):
        return 1

class Service:
    body_field: Ranker

    def __init__(self, injected: Ranker):
        self.typed_attr: Ranker = build()
        self.injected = injected
        r = injected
        r.rank()

    def execute(self):
        self.body_field.rank()
        self.typed_attr.rank()
        self.injected.rank()

SVC: Service

def main():
    pl.col("a")
    pytest.approx(1.0)
    SVC.execute()
    body_field.rank()
"#;
    let parsed = parse_source(src.as_bytes(), Lang::Python).unwrap();
    let fx = extract_file(0, "svc.py", src.as_bytes(), &parsed);
    let res = resolve(vec![fx], Default::default());

    let rank_count = res
        .edges
        .iter()
        .filter(|e| {
            e.kind == EdgeKind::Calls
                && e.confidence == Confidence::Extracted
                && e.dst_name == "svc.Ranker.rank"
        })
        .count();
    assert_eq!(
        rank_count, 4,
        "4 Ranker.rank() calls expected: {:?}",
        res.edges
    );
    assert!(
        res.edges.iter().any(|e| e.kind == EdgeKind::Calls
            && e.confidence == Confidence::Extracted
            && e.dst_name == "svc.Service.execute"),
        "module-level annotated SVC.execute() must resolve"
    );
    // body_field.rank() at module scope must NOT resolve via class-body field leak.
    assert!(
        res.unresolved_calls
            .iter()
            .any(|u| u.name == "body_field.rank"),
        "class-body field must not leak to module scope: {:?}",
        res.unresolved_calls
    );
}

#[test]
fn tier2_return_types_mro_fields_property_multihop_and_reexports() {
    let ranker_src = "class Leaf:\n    def ping(self):\n        return 1\n\nclass Ranker:\n    leaf: Leaf\n    def rank(self):\n        return 2\n";
    let base_src = "from pkg.ranker import Ranker\n\ndef make_ranker() -> Ranker:\n    return Ranker()\n\nclass Base:\n    def __init__(self):\n        self.r = make_ranker()\n\n    @property\n    def prop(self) -> Ranker:\n        return self.r\n";
    let init_src = "from .ranker import *\n";
    let app_src = "import pkg\nfrom pkg import Ranker\nfrom base import Base, make_ranker\n\nclass Sub(Base):\n    def go(self):\n        x = make_ranker()\n        x.rank()\n        self.r.rank()\n        self.prop.rank()\n        self.r.leaf.ping()\n        pkg.Ranker.rank()\n";
    let files = [
        ("pkg/ranker.py", ranker_src),
        ("base.py", base_src),
        ("pkg/__init__.py", init_src),
        ("app.py", app_src),
    ]
    .into_iter()
    .enumerate()
    .map(|(i, (p, s))| {
        let parsed = parse_source(s.as_bytes(), Lang::Python).unwrap();
        extract_file(i, p, s.as_bytes(), &parsed)
    })
    .collect();
    let res = resolve(files, Default::default());
    let rank_calls: Vec<usize> = res
        .edges
        .iter()
        .filter(|e| {
            e.kind == EdgeKind::Calls
                && e.confidence == Confidence::Extracted
                && e.dst_name == "pkg.ranker.Ranker.rank"
        })
        .map(|e| e.site_line)
        .collect();
    assert_eq!(
        rank_calls,
        vec![8, 9, 10, 12],
        "expected x.rank, self.r.rank, self.prop.rank, pkg.Ranker.rank: {rank_calls:?}"
    );
    assert!(
        res.edges.iter().any(|e| e.kind == EdgeKind::Calls
            && e.confidence == Confidence::Extracted
            && e.dst_name == "pkg.ranker.Leaf.ping"),
        "multi-hop self.r.leaf.ping() must resolve"
    );
    assert!(
        res.edges.iter().any(|e| e.kind == EdgeKind::Imports
            && e.confidence == Confidence::Extracted
            && e.dst_name == "pkg.Ranker"),
        "from pkg import Ranker via star re-export must emit Imports edge"
    );
    assert!(
        res.edges
            .iter()
            .any(|e| e.kind == EdgeKind::TypeRef && e.dst_name == "pkg.ranker.Ranker"),
        "TypeRef edge to Ranker must be emitted"
    );
}

#[test]
fn tier2_carried_index_preserves_return_types_fields_and_reexports() {
    let carried = CarriedIndex {
        symbols: vec![
            CarriedSymbol {
                row_id: 1,
                kind: SymbolKind::Module,
                name: "base".into(),
                qualname: "base".into(),
                module: "base".into(),
                return_type: String::new(),
            },
            CarriedSymbol {
                row_id: 2,
                kind: SymbolKind::Class,
                name: "Ranker".into(),
                qualname: "base.Ranker".into(),
                module: "base".into(),
                return_type: String::new(),
            },
            CarriedSymbol {
                row_id: 3,
                kind: SymbolKind::Method,
                name: "rank".into(),
                qualname: "base.Ranker.rank".into(),
                module: "base".into(),
                return_type: String::new(),
            },
            CarriedSymbol {
                row_id: 4,
                kind: SymbolKind::Function,
                name: "make_ranker".into(),
                qualname: "base.make_ranker".into(),
                module: "base".into(),
                return_type: "base.Ranker".into(),
            },
            CarriedSymbol {
                row_id: 5,
                kind: SymbolKind::Class,
                name: "CarriedBase".into(),
                qualname: "base.CarriedBase".into(),
                module: "base".into(),
                return_type: String::new(),
            },
            CarriedSymbol {
                row_id: 6,
                kind: SymbolKind::Method,
                name: "prop".into(),
                qualname: "base.CarriedBase.prop".into(),
                module: "base".into(),
                return_type: "base.Ranker".into(),
            },
            CarriedSymbol {
                row_id: 7,
                kind: SymbolKind::Module,
                name: "pkg".into(),
                qualname: "pkg".into(),
                module: "pkg".into(),
                return_type: String::new(),
            },
        ],
        inherits: vec![],
        class_fields: vec![("base.CarriedBase".into(), "r".into(), "base.Ranker".into())],
        reexports: vec![("pkg".into(), "Ranker".into(), 2)],
        module_stars: vec![],
    };
    let app_src = "from base import CarriedBase, make_ranker\nimport pkg\n\nclass Sub(CarriedBase):\n    def run(self):\n        x = make_ranker()\n        x.rank()\n        self.r.rank()\n        self.prop.rank()\n        pkg.Ranker.rank()\n";
    let parsed = parse_source(app_src.as_bytes(), Lang::Python).unwrap();
    let fx = extract_file(0, "app.py", app_src.as_bytes(), &parsed);
    let res = resolve(vec![fx], carried);
    let rank_lines: Vec<usize> = res
        .edges
        .iter()
        .filter(|e| {
            e.kind == EdgeKind::Calls
                && e.confidence == Confidence::Extracted
                && e.dst_name == "base.Ranker.rank"
        })
        .map(|e| e.site_line)
        .collect();
    assert_eq!(
        rank_lines,
        vec![7, 8, 9, 10],
        "carried return_type, class_fields, @property, and reexports must resolve: {rank_lines:?}"
    );
}

#[test]
fn resolve_target_class_chained_factory_respects_site_line() {
    let src = "class B:\n    def run(self):\n        pass\n\nclass A:\n    def make_b(self) -> B:\n        return B()\n\ndef make_a() -> A:\n    return A()\n\ndef flow():\n    a = make_a()\n    b = a.make_b()\n    b.run()\n    b = unknown()\n    b.run()\n";
    let p = parse_source(src.as_bytes(), Lang::Python).unwrap();
    let fx = extract_file(0, "chain.py", src.as_bytes(), &p);
    let res = resolve(vec![fx], Default::default());
    let run_lines: Vec<usize> = res
        .edges
        .iter()
        .filter(|e| {
            e.kind == EdgeKind::Calls
                && e.confidence == Confidence::Extracted
                && e.dst_name == "chain.B.run"
        })
        .map(|e| e.site_line)
        .collect();
    assert_eq!(
        run_lines,
        vec![15],
        "chained factory b = a.make_b() resolves b.run() at line 15 and drops after unknown() at line 17: {run_lines:?}"
    );
}
