use super::common::*;

#[test]
fn cli_memory_healing_hygiene_triage_retire_restore_and_cas() {
    let repo = temp_repo("mem-healing");
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::write(repo.join("src/lib.rs"), "pub fn alpha() {}\n").unwrap();
    let out = eg(&repo).arg("build").output().unwrap();
    assert_eq!(out.status.code(), Some(0));

    // 1. Durable write to .tmp/ is rejected with exit 2.
    let (code, _, stderr) = mem_out(
        &repo,
        &[
            "write",
            "tmp-note",
            "--body",
            "scratch path",
            "--kind",
            "gotcha",
            "--refs",
            ".tmp/scratch.py",
        ],
    );
    assert_eq!(code, 2, "expected exit 2 for .tmp ref: {stderr}");

    // ENGRAM_MEM_HYGIENE=0 downgrades policy rejection to stderr warning.
    let out = eg(&repo)
        .env("ENGRAM_MEM_HYGIENE", "0")
        .args([
            "mem",
            "write",
            "tmp-note",
            "--body",
            "scratch path",
            "--kind",
            "gotcha",
            "--refs",
            ".tmp/scratch.py",
        ])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
    let warn_stderr = String::from_utf8_lossy(&out.stderr);
    assert!(warn_stderr.contains("warning: hygiene"), "{warn_stderr}");

    // 2. Write a valid note, then break its ref and triage.
    let (code, _, stderr) = mem_out(
        &repo,
        &[
            "write",
            "alpha-note",
            "--body",
            "alpha behavior",
            "--kind",
            "fact",
            "--refs",
            "src/lib.rs",
        ],
    );
    assert_eq!(code, 0, "{stderr}");
    std::fs::remove_file(repo.join("src/lib.rs")).unwrap();
    let out = eg(&repo).arg("build").output().unwrap();
    assert_eq!(out.status.code(), Some(0));

    let (code, stdout, stderr) = mem_out(&repo, &["triage", "--json", "--no-git"]);
    assert_eq!(code, 0, "triage failed: {stderr}");
    let json_start = stdout.find('{').expect("triage JSON object");
    let parsed: serde_json::Value = serde_json::from_str(&stdout[json_start..]).unwrap();
    let items = parsed["items"].as_array().unwrap();
    assert!(items.iter().any(|i| i["key"] == "alpha-note"));

    // 3. Stale --expected-rev conflicts (exit 2).
    let (code, _, stderr) = mem_out(
        &repo,
        &[
            "write",
            "alpha-note",
            "--body",
            "updated",
            "--kind",
            "fact",
            "--expected-rev",
            "99",
        ],
    );
    assert_eq!(code, 2, "expected exit 2 conflict: {stderr}");

    // 4. Soft-retire with --expected-rev 1, inspect --include-superseded, then restore.
    let (code, stdout, stderr) = mem_out(
        &repo,
        &["delete", "alpha-note", "--retire", "--expected-rev", "1"],
    );
    assert_eq!(code, 0, "retire failed: {stderr}");
    assert!(stdout.contains("retired"), "{stdout}");

    let (code, _, _) = mem_out(&repo, &["get", "alpha-note"]);
    assert_eq!(code, 1, "retired note must be hidden from default get");

    let (code, stdout, stderr) = mem_out(&repo, &["get", "alpha-note", "--include-superseded"]);
    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.contains("[retired]"), "{stdout}");

    let (code, stdout, stderr) = mem_out(&repo, &["restore", "alpha-note"]);
    assert_eq!(code, 0, "restore failed: {stderr}");
    assert!(stdout.contains("restored"), "{stdout}");

    let (code, _, _) = mem_out(&repo, &["get", "alpha-note"]);
    assert_eq!(code, 0, "restored note must be active again");
}
