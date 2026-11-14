//! Dashboard touch tests.

use super::*;

#[test]
fn dashboard_api_memory_touch_success_and_error() {
    let repo = super::super::common::temp_repo("dash-touch-api");
    super::super::common::mem_write(&repo, "touch-key", "body content", "gotcha");

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();

    let temp_log = repo.join("usage.log");
    let log_clone = temp_log.clone();
    let repo_clone = repo.clone();
    let handle = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let _ = handle_client(stream, &log_clone, Some(&repo_clone), false);
    });

    let store_id = std::fs::canonicalize(repo.join(".engram/memory.db"))
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let mut client = TcpStream::connect(addr).unwrap();
    let body = format!(r#"{{"key":"touch-key","store_id":"{store_id}","rev":1}}"#);
    let req = format!(
        "POST /api/memory/touch HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
        body.len(),
        body
    );
    client.write_all(req.as_bytes()).unwrap();
    let mut resp = String::new();
    client.read_to_string(&mut resp).unwrap();
    assert!(resp.starts_with("HTTP/1.1 200 OK"), "{resp}");
    assert!(resp.contains("\"ok\":true"), "{resp}");
    assert!(resp.contains("\"rev\":1"), "{resp}");
    assert!(resp.contains("\"store_id\":"), "{resp}");
    handle.join().unwrap();

    let _ = std::fs::remove_dir_all(&repo);
}

/// A dead (expired) revision is unreachable by the touch pipeline, matching
/// the CLI's `mem touch`: the current-rev probe filters on `expires_at` and
/// the UPDATE re-checks it, so an expired key 404s instead of re-anchoring a
/// dead row with a fresh `anchored_at`.
#[test]
fn dashboard_touch_rejects_expired_key() {
    let repo = super::super::common::temp_repo("dash-touch-expired");
    let (code, stdout, stderr) = super::super::common::mem_out(
        &repo,
        &[
            "write",
            "touch-key",
            "--body",
            "session body",
            "--kind",
            "gotcha",
            "--scope",
            "session",
            "--ttl",
            "1",
        ],
    );
    assert_eq!(code, 0, "{stdout}{stderr}");
    // Outlive the 1-second session TTL before the dashboard probes it.
    std::thread::sleep(std::time::Duration::from_millis(1_500));

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let temp_log = repo.join("usage.log");
    let log_clone = temp_log.clone();
    let repo_clone = repo.clone();
    let handle = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let _ = handle_client(stream, &log_clone, Some(&repo_clone), false);
    });
    let store_id = std::fs::canonicalize(repo.join(".engram/memory.db"))
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let mut client = TcpStream::connect(addr).unwrap();
    let body = format!(r#"{{"key":"touch-key","store_id":"{store_id}","rev":1}}"#);
    let req = format!(
        "POST /api/memory/touch HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
        body.len(),
        body
    );
    client.write_all(req.as_bytes()).unwrap();
    let mut resp = String::new();
    client.read_to_string(&mut resp).unwrap();
    assert!(resp.starts_with("HTTP/1.1 404"), "{resp}");
    assert!(resp.contains("\"ok\":false"), "{resp}");
    assert!(
        resp.contains("unknown key 'touch-key'"),
        "the expired row must not surface as touchable: {resp}"
    );
    handle.join().unwrap();

    let _ = std::fs::remove_dir_all(&repo);
}

#[test]
fn dashboard_touch_rejects_bad_content_type_stale_rev_and_unknown_store() {
    let repo = super::super::common::temp_repo("dash-touch-status");
    super::super::common::mem_write(&repo, "touch-key", "body content", "gotcha");
    let store_id = std::fs::canonicalize(repo.join(".engram/memory.db"))
        .unwrap()
        .to_string_lossy()
        .into_owned();

    let send = |body: String, content_type: &str, expect: &str| {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let temp_log = repo.join("usage.log");
        let log_clone = temp_log.clone();
        let repo_clone = repo.clone();
        let handle = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let _ = handle_client(stream, &log_clone, Some(&repo_clone), false);
        });
        let mut client = TcpStream::connect(addr).unwrap();
        let req = format!(
            "POST /api/memory/touch HTTP/1.1\r\nHost: localhost\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        client.write_all(req.as_bytes()).unwrap();
        let mut resp = String::new();
        client.read_to_string(&mut resp).unwrap();
        assert!(resp.starts_with(expect), "{resp}");
        handle.join().unwrap();
    };

    // Missing/incorrect Content-Type -> 415.
    send(
        format!(r#"{{"key":"touch-key","store_id":"{store_id}","rev":1}}"#),
        "text/plain",
        "HTTP/1.1 415",
    );
    // Stale revision -> 409 with the current rev named.
    send(
        format!(r#"{{"key":"touch-key","store_id":"{store_id}","rev":99}}"#),
        "application/json",
        "HTTP/1.1 409",
    );
    // Unknown store -> 404, never a silent cross-store fallback.
    send(
        r#"{"key":"touch-key","store_id":"/nonexistent/store/memory.db","rev":1}"#.to_string(),
        "application/json",
        "HTTP/1.1 404",
    );
    let _ = std::fs::remove_dir_all(&repo);
}

/// I-16: global mode never offers a scratch store for mutation — the touch
/// 404's `available_stores` agrees with what the read endpoints show, while
/// explicit-root local mode keeps its store.
#[test]
fn dashboard_touch_global_hides_tmp_stores() {
    use super::super::common::mem_write;

    // Durable store outside the OS temp tree (gitignored target dir): the
    // scratch entry must be the only one the global filter removes.
    let seq = super::super::common::TEMP_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let real = std::path::Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("eg-touch-stable-{}-{seq}", std::process::id()));
    let _ = std::fs::remove_dir_all(&real);
    std::fs::create_dir_all(&real).unwrap();
    // `RepoRef::resolve` climbs to the enclosing work tree, and this
    // gitignored target directory sits inside one: an explicit `.git` stops
    // the climb at the fixture instead of the enclosing checkout's store.
    let mut command = std::process::Command::new("git");
    for key in std::env::vars().map(|(k, _)| k).collect::<Vec<_>>() {
        if key.starts_with("GIT_") {
            command.env_remove(key);
        }
    }
    let init = command
        .arg("-C")
        .arg(&real)
        .args(["init", "-q"])
        .status()
        .unwrap();
    assert!(init.success());
    let (code, stdout, stderr) = mem_write(&real, "touch-key", "body content", "gotcha");
    assert_eq!(code, 0, "{stdout}{stderr}");
    assert!(
        real.join(".engram/memory.db").is_file(),
        "the note must land in the fixture store, not the enclosing checkout"
    );

    // Scratch store under the temp tree with a real memory.db: discoverable
    // when unfiltered, hidden by the global ephemeral gate.
    let scratch = std::env::temp_dir().join(format!("eg-touch-scratch-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&scratch);
    std::fs::create_dir_all(scratch.join(".engram")).unwrap();
    std::fs::write(scratch.join(".engram/memory.db"), b"x").unwrap();

    let real_store_id = std::fs::canonicalize(real.join(".engram/memory.db"))
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let scratch_store_id = std::fs::canonicalize(scratch.join(".engram/memory.db"))
        .unwrap()
        .to_string_lossy()
        .into_owned();

    let scratch_entry = format!(
        r#"{{"tool":"mem_write","exit_code":0,"repo":"{}","repo_name":"probe-store"}}"#,
        scratch.display()
    );
    let real_entry = format!(
        r#"{{"tool":"mem_write","exit_code":0,"repo":"{}","repo_name":"stable"}}"#,
        real.display()
    );

    let real_for_handler = real.clone();
    let send_touch = |log_body: String, global: bool| {
        let dir = super::super::common::temp_repo("dash-touch-global-log");
        let log = dir.join("usage.log");
        std::fs::write(&log, log_body.as_bytes()).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let log_clone = log.clone();
        let real_clone = real_for_handler.clone();
        let handle = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let _ = if global {
                handle_client(stream, &log_clone, None, true)
            } else {
                handle_client(stream, &log_clone, Some(&real_clone), false)
            };
        });
        let mut client = TcpStream::connect(addr).unwrap();
        let body = r#"{"key":"touch-key","store_id":"unknown-store","rev":1}"#;
        let req = format!(
            "POST /api/memory/touch HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        client.write_all(req.as_bytes()).unwrap();
        let mut resp = String::new();
        client.read_to_string(&mut resp).unwrap();
        handle.join().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        resp
    };

    let global_log = format!("{scratch_entry}\n{real_entry}\n");
    let resp = send_touch(global_log.clone(), true);
    assert!(resp.starts_with("HTTP/1.1 404"), "{resp}");
    assert!(
        !resp.contains(&scratch_store_id),
        "global touch must omit the scratch store: {resp}"
    );
    assert!(
        resp.contains(&real_store_id),
        "global touch still lists the durable store: {resp}"
    );

    // Explicit-root local mode: no scratch filtering, the real store is
    // offered for mutation.
    let resp = send_touch(global_log, false);
    assert!(resp.starts_with("HTTP/1.1 404"), "{resp}");
    assert!(
        resp.contains(&real_store_id),
        "local touch lists the root's store: {resp}"
    );

    let _ = std::fs::remove_dir_all(&real);
    let _ = std::fs::remove_dir_all(&scratch);
}
