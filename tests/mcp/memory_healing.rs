use serde_json::json;

use super::common::{init_and_list_tools, mem_fixture, result_text, rpc, spawn_with};

#[test]
fn mcp_memory_healing_guards_unknown_keys_confirm_cas_and_triage() {
    let repo = mem_fixture("healing");
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::write(repo.join("src/lib.rs"), "pub fn beta() {}\n").unwrap();

    let mut srv = spawn_with(&repo, true);
    let _names = init_and_list_tools(&mut srv);

    // 1. Unknown argument on mem_* is rejected.
    let bad = rpc(
        &mut srv,
        3,
        "tools/call",
        json!({
            "name": "mem_write",
            "arguments": {
                "key": "k",
                "body": "b",
                "kind": "fact",
                "bogus_flag": true
            }
        }),
    );
    assert_eq!(bad["result"]["isError"], true, "{bad}");
    assert!(result_text(&bad).contains("unknown argument 'bogus_flag'"));

    // 2. Write note via MCP.
    let wr = rpc(
        &mut srv,
        4,
        "tools/call",
        json!({
            "name": "mem_write",
            "arguments": {
                "key": "beta-note",
                "body": "beta note",
                "kind": "fact",
                "refs": "src/lib.rs"
            }
        }),
    );
    assert_eq!(wr["result"]["isError"], false, "{wr}");

    // 3. mem_retire without confirm=true is rejected.
    let no_conf = rpc(
        &mut srv,
        5,
        "tools/call",
        json!({"name": "mem_retire", "arguments": {"key": "beta-note"}}),
    );
    assert_eq!(no_conf["result"]["isError"], true, "{no_conf}");

    // 4. mem_retire with confirm=true and expected_rev=1 succeeds.
    let retire = rpc(
        &mut srv,
        6,
        "tools/call",
        json!({
            "name": "mem_retire",
            "arguments": {"key": "beta-note", "confirm": true, "expected_rev": 1}
        }),
    );
    assert_eq!(retire["result"]["isError"], false, "{retire}");
    assert!(result_text(&retire).contains("retired"));

    // 5. mem_get with include_superseded=true shows [retired].
    let get_ret = rpc(
        &mut srv,
        7,
        "tools/call",
        json!({
            "name": "mem_get",
            "arguments": {"key": "beta-note", "include_superseded": true}
        }),
    );
    assert_eq!(get_ret["result"]["isError"], false, "{get_ret}");
    assert!(result_text(&get_ret).contains("[retired]"));

    // 6. mem_restore brings it back.
    let restore = rpc(
        &mut srv,
        8,
        "tools/call",
        json!({"name": "mem_restore", "arguments": {"key": "beta-note"}}),
    );
    assert_eq!(restore["result"]["isError"], false, "{restore}");
    assert!(result_text(&restore).contains("restored"));

    // 7. Break ref, rebuild index, and run mem_triage.
    std::fs::remove_file(repo.join("src/lib.rs")).unwrap();
    let bld = rpc(
        &mut srv,
        9,
        "tools/call",
        json!({"name": "build", "arguments": {}}),
    );
    assert_eq!(bld["result"]["isError"], false, "{bld}");
    let triage = rpc(
        &mut srv,
        10,
        "tools/call",
        json!({"name": "mem_triage", "arguments": {"no_git": true}}),
    );
    assert_eq!(triage["result"]["isError"], false, "{triage}");
    let txt = result_text(&triage);
    let json_start = txt.find('{').expect("triage JSON");
    let parsed: serde_json::Value = serde_json::from_str(&txt[json_start..]).unwrap();
    assert_eq!(parsed["items"][0]["key"], "beta-note");
}
