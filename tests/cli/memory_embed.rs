use super::common::*;

#[cfg(unix)]
#[test]
fn mem_embed_plumbing_round_trip() {
    use std::os::unix::fs::PermissionsExt;

    let repo = temp_repo("mem-embed-plumbing");

    // eg-judge stub: the embed command answers a fixed 4-dim vector; other
    // subcommands answer a duplicate verdict (unused here).
    let bin = repo.join("judge.sh");
    std::fs::write(
        &bin,
        "#!/bin/sh\nif [ \"$1\" = \"embed\" ]; then\n  if [ \"$2\" = \"--stdin\" ]; then\n    while IFS= read -r _line; do\n      printf '%s\\n' '{\"dim\":4,\"embedding\":[0.5,0.5,0.5,0.5]}'\n    done\n    exit 0\n  fi\n  printf '%s\\n' '{\"dim\":4,\"embedding\":[0.5,0.5,0.5,0.5]}'\n  exit 0\nfi\nprintf '%s\\n' '{\"decision\":\"duplicate\",\"probability\":0.9}'\n",
    )
    .unwrap();
    let mut perms = std::fs::metadata(&bin).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&bin, perms).unwrap();
    let model = repo.join("text-model");
    std::fs::create_dir_all(&model).unwrap();
    std::fs::write(model.join("lazy.json"), "{}\n").unwrap();

    let write = eg(&repo)
        .env("EG_JUDGE_BIN", &bin)
        .env("EG_JUDGE_TEXT_MODEL", &model)
        .args([
            "mem",
            "write",
            "embed_note",
            "--body",
            "embedding plumbing",
            "--kind",
            "fact",
        ])
        .output()
        .unwrap();
    assert_eq!(
        write.status.code(),
        Some(0),
        "{}{}",
        String::from_utf8_lossy(&write.stdout),
        String::from_utf8_lossy(&write.stderr)
    );

    // The stored row carries a non-NULL embedding blob.
    let stored = eg(&repo)
        .args(["mem", "get", "embed_note"])
        .output()
        .unwrap();
    assert_eq!(
        stored.status.code(),
        Some(0),
        "{}{}",
        String::from_utf8_lossy(&stored.stdout),
        String::from_utf8_lossy(&stored.stderr)
    );
    let blob = std::process::Command::new("sqlite3")
        .arg(repo.join(".engram/memory.db"))
        .arg("SELECT length(embedding) FROM memories WHERE key = 'embed_note'")
        .output()
        .unwrap();
    let length = String::from_utf8_lossy(&blob.stdout).trim().to_string();
    assert_eq!(length, "16", "4 f32 LE bytes: {length}");

    // Query with full embedding coverage: the sem feature is active and the
    // lexical contract still renders the note.
    let query = eg(&repo)
        .env("EG_JUDGE_BIN", &bin)
        .env("EG_JUDGE_TEXT_MODEL", &model)
        .args(["mem", "query", "embedding"])
        .output()
        .unwrap();
    assert_eq!(
        query.status.code(),
        Some(0),
        "{}{}",
        String::from_utf8_lossy(&query.stdout),
        String::from_utf8_lossy(&query.stderr)
    );
    let stdout = String::from_utf8_lossy(&query.stdout);
    assert!(stdout.contains("embed_note"), "{stdout}");

    let unembedded = eg(&repo)
        .env("HOME", &repo)
        .env_remove("EG_JUDGE_BIN")
        .env_remove("EG_JUDGE_TEXT_MODEL")
        .args([
            "mem",
            "write",
            "unembedded_note",
            "--body",
            "needs backfill",
            "--kind",
            "fact",
        ])
        .output()
        .unwrap();
    assert_eq!(
        unembedded.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&unembedded.stderr)
    );
    // Reindex --embed backfills: the report counts embedded notes.
    let reindex = eg(&repo)
        .env("EG_JUDGE_BIN", &bin)
        .env("EG_JUDGE_TEXT_MODEL", &model)
        .args(["mem", "reindex", "--embed"])
        .output()
        .unwrap();
    assert_eq!(
        reindex.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&reindex.stderr)
    );
    let out = format!(
        "{}{}",
        String::from_utf8_lossy(&reindex.stdout),
        String::from_utf8_lossy(&reindex.stderr)
    );
    assert!(out.contains("embedded 1 notes"), "{out}");
}

#[cfg(unix)]
#[test]
fn mem_embed_degrades_without_a_text_model() {
    let repo = temp_repo("mem-embed-unconfigured");
    let write = eg(&repo)
        .env("HOME", &repo)
        .env_remove("EG_JUDGE_BIN")
        .env_remove("EG_JUDGE_TEXT_MODEL")
        .args([
            "mem",
            "write",
            "plain_note",
            "--body",
            "no model here",
            "--kind",
            "fact",
        ])
        .output()
        .unwrap();
    assert_eq!(
        write.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&write.stderr)
    );
    // With no EG_JUDGE_TEXT_MODEL the write path skips embedding entirely and
    // the store stays on the lexical-only contract.
    let blob = std::process::Command::new("sqlite3")
        .arg(repo.join(".engram/memory.db"))
        .arg("SELECT embedding IS NULL FROM memories WHERE key = 'plain_note'")
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&blob.stdout).trim(), "1");
}
