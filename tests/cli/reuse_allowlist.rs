//! Reuse allowlist reference-currency tests: a ref-bearing `reuse-allow/`
//! note suppresses only while every referenced file is indexed, unchanged on
//! disk, and no newer than the note's anchor. Rebuilding the index after a
//! referenced file changed re-reports the duplicate until `mem touch`
//! refreshes the anchor.

use super::common::*;

fn duplicate_repo(name: &str) -> TempRepo {
    let repo = temp_repo(name);
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
    build(&repo);
    repo
}

fn write_allow_note(repo: &TempRepo, refs: Option<&str>) {
    let mut args = vec![
        "mem",
        "write",
        "reuse-allow/b.parse_token->a.parse_token",
        "--kind",
        "decision",
        "--body",
        "intentional duplicate",
    ];
    if let Some(r) = refs {
        args.extend(["--refs", r]);
    }
    let out = eg(repo).args(args).output().unwrap();
    assert_eq!(out.status.code(), Some(0), "{out:?}");
}

#[test]
fn allowlist_note_requires_touch_after_referenced_file_changes() {
    let repo = duplicate_repo("reuse-allow-currency");
    write_allow_note(&repo, Some("b.py"));
    // Move the referenced file's mtime past the note's anchor second: the
    // same-second write would compare equal and read as still valid.
    std::thread::sleep(std::time::Duration::from_millis(1100));
    write(
        &repo,
        "b.py",
        "def parse_token(token: str):\n    return token.strip() + '!'\n",
    );
    build(&repo);

    let stale = eg(&repo).args(["reuse"]).output().unwrap();
    let stale_stdout = String::from_utf8_lossy(&stale.stdout);
    assert!(
        stale_stdout.contains("symbol 'b.parse_token'"),
        "{stale_stdout}"
    );
    assert!(!stale_stdout.contains("suppressed"), "{stale_stdout}");

    let (code, stdout, stderr) = mem_out(
        &repo,
        &["touch", "reuse-allow/b.parse_token->a.parse_token"],
    );
    assert_eq!(code, 0, "eg mem touch failed: {stdout}{stderr}");

    let current = eg(&repo).args(["reuse"]).output().unwrap();
    let current_stdout = String::from_utf8_lossy(&current.stdout);
    assert!(
        current_stdout.contains("0 duplication candidate(s) found"),
        "{current_stdout}"
    );
    assert!(current_stdout.contains("1 suppressed"), "{current_stdout}");
}

#[test]
fn moved_ref_does_not_suppress_through_suffix_match() {
    let repo = duplicate_repo("reuse-allow-moved");
    write(&repo, "src/old.py", "# placeholder\n");
    build(&repo);
    // The rename keeps the file's creation mtime, so the vendor copy is older
    // than the note written afterwards: a suffix-only match would read as
    // valid and suppress, which the exact-row precondition must prevent.
    std::fs::create_dir_all(repo.join("vendor/src")).unwrap();
    std::fs::rename(repo.join("src/old.py"), repo.join("vendor/src/old.py")).unwrap();
    write_allow_note(&repo, Some("src/old.py"));
    build(&repo);

    let out = eg(&repo).args(["reuse"]).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("symbol 'b.parse_token'"), "{stdout}");
    assert!(!stdout.contains("suppressed"), "{stdout}");
}

#[test]
fn refless_allowlist_note_still_suppresses() {
    let repo = duplicate_repo("reuse-allow-refless");
    write_allow_note(&repo, None);

    let out = eg(&repo).args(["reuse"]).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("0 duplication candidate(s) found"),
        "{stdout}"
    );
    assert!(stdout.contains("1 suppressed"), "{stdout}");
}
