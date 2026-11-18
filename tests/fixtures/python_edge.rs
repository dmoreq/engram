//! Python annotation and inherit-edge fixtures: annotated-assignment
//! receiver typing, untyped-receiver negatives, and the AMBIGUOUS base edge.

use engram::extract::{extract_file, resolve, Confidence, EdgeKind, EdgeSiteKind};
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
