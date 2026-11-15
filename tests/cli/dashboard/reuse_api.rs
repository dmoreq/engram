//! Reuse-API payload tests: per-repo candidate pools, sample caps, and the
//! provenance/degraded states the Anti-Reinvention tables render.

use super::*;

#[test]
fn dashboard_reuse_api_reports_per_repo_pools() {
    // The pool is the repository's full candidate count before the 25-cap;
    // the payload carries it per repo so the UI can render "sampled / pool"
    // instead of a capped sample alone. `stable_repo` keeps the journaled
    // root outside the OS temp tree, which the dashboard hides as scratch.
    let repo = stable_repo("reuse-pools");
    write(
        &repo,
        "a.py",
        "def parse_token(token: str):\n    return token.strip()\n",
    );
    write(
        &repo,
        "b.py",
        "def parse_token(token: str):\n    return token.strip()\n",
    );
    let (build_code, build_out, build_err) = build(&repo);
    assert_eq!(build_code, 0, "{build_err}");
    eprintln!("diag-build: {build_out}{build_err}");
    // Mirror the scan's provenance gate: what does the fixture's own git see?
    let probe = std::process::Command::new("git")
        .arg("-C")
        .arg(&repo)
        .args(["ls-files", "-z", "--", ":(icase).engram"])
        .output()
        .unwrap();
    eprintln!(
        "diag-git: status={} tracked_bytes={}",
        probe.status,
        probe.stdout.len(),
    );

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    // The journal reader resolves `usage.log` inside the log path's PARENT
    // directory, so the fixture gets its own directory like every dashboard
    // test that feeds entries to a route.
    let log_dir = std::env::temp_dir().join(format!("test_usage_pools_{}", std::process::id()));
    std::fs::create_dir_all(&log_dir).unwrap();
    let temp_log = log_dir.join("usage.log");
    let repo_str = repo.to_string_lossy().into_owned();
    std::fs::write(
        &temp_log,
        format!(
            // Journal lines are newline-terminated: a torn tail is left unread.
            r#"{{"tool":"search","exit_code":0,"repo":"{repo_str}","repo_name":"reuse-pools","shown":1,"latency_us":100}}"#,
        ) + "\n",
    )
    .unwrap();
    let log_clone = temp_log.clone();
    let handle = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let _ = handle_client(stream, &log_clone, None, false);
    });

    let mut client = TcpStream::connect(addr).unwrap();
    client
        .write_all(b"GET /api/reuse HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .unwrap();
    let mut resp = String::new();
    client.read_to_string(&mut resp).unwrap();
    assert!(resp.starts_with("HTTP/1.1 200 OK"), "{resp}");
    let body_start = resp.find("\r\n\r\n").unwrap() + 4;
    let json_val: serde_json::Value = serde_json::from_str(&resp[body_start..]).unwrap();

    let pools = &json_val["summary"]["repo_candidate_pools"];
    assert!(pools.is_object(), "{resp}");
    // The fixture's own row: its label is the path basename, and its pool
    // must be present and non-empty. Other rows (e.g. the working directory's
    // own index) may coexist and are ignored here. Pools are keyed by the
    // canonical repository PATH — collision-proof where labels are not.
    let pool = pools[&repo_str]
        .as_u64()
        .unwrap_or_else(|| panic!("fixture pool missing for {repo_str}: {pools}"));
    assert!(pool >= 1, "{pools}");
    // The sampled list never exceeds the pool it was drawn from.
    let candidates = json_val["candidates"].as_array().unwrap();
    let sampled = candidates
        .iter()
        .filter(|c| c["repo_path"] == repo_str)
        .count() as u64;
    assert_eq!(sampled, pool.min(25), "{candidates:?}");

    // JSON contract for the closure line: the client reads exactly these
    // keys (a drift blanks the subtitle instead of erroring).
    let closure = &json_val["summary"]["resolved_twins"];
    assert!(closure["resolved"].is_u64(), "{closure}");
    assert!(closure["surfaced"].is_u64(), "{closure}");

    handle.join().unwrap();
    let _ = std::fs::remove_dir_all(&log_dir);
}
