use super::*;

#[test]
fn mem_export_scan_param_and_schema_are_wired() {
    let spec = TOOL_SPECS.iter().find(|t| t.name == "mem_export").unwrap();
    assert!(spec.params.iter().any(|p| p.name == "scan"));
    let req = from_tool_args("mem_export", &json!({"scan": true})).unwrap();
    assert!(matches!(req, CommandRequest::MemExport { scan: true, .. }));
    let reuse = TOOL_SPECS.iter().find(|t| t.name == "reuse").unwrap();
    let limit = reuse.params.iter().find(|p| p.name == "limit").unwrap();
    assert!(matches!(limit.kind, ParamKind::Int { min: 0, .. }));
}

#[test]
fn call_tool_forwards_stderr_for_non_export_and_strips_export_caveat() {
    let dir = crate::db::test_support::temp_root("mcp-stderr-fwd");
    crate::db::test_support::make_dir_all(dir.join(".engram")).unwrap();
    for (key, body) in [("note-a", "alpha note"), ("note-b", "beta note")] {
        let (t, err, code, _) = call_tool(
            &dir,
            "mem_write",
            &json!({"key": key, "body": body, "kind": "fact"}),
            true,
        );
        assert!(!err && code == 0, "{t}");
    }
    let (queried, err, code, _) = call_tool(
        &dir,
        "mem_query",
        &json!({"all": true, "limit": 1, "local": true}),
        true,
    );
    assert!(
        !err && code == 0 && queried.contains("[eg] capped at 1 results"),
        "{queried}"
    );
    let (plain, err, code, _) = call_tool(&dir, "mem_export", &json!({}), true);
    assert!(
        !err && code == 0 && !plain.contains("eg: memory"),
        "{plain}"
    );
    crate::db::test_support::remove_tree(&dir);
}
