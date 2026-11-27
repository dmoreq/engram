//! Judge batch contract: one NDJSON payload, one subprocess, per-line
//! verdicts, recipe pass-through, and structured journal counts.

use super::common::*;

#[cfg(unix)]
fn install_stub(repo: &Path, script: &str) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;

    let bin = repo.join("judge.sh");
    std::fs::write(&bin, script).unwrap();
    let mut perms = std::fs::metadata(&bin).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&bin, perms).unwrap();
    std::fs::create_dir_all(repo.join("model")).unwrap();
    std::fs::write(repo.join("model/lazy.json"), "{\"head_layers\":1}\n").unwrap();
    bin
}

/// Per-line stub: echoes one verdict per stdin line so a two-pair batch
/// exercises the NDJSON contract end to end.
#[cfg(unix)]
const LINE_STUB: &str = "#!/bin/sh\nwhile IFS= read -r line; do\n  case \"$line\" in\n    *token_value*) printf '%s\\n' '{\"decision\":\"distinct\",\"probability\":0.4}' ;;\n    *) printf '%s\\n' '{\"decision\":\"duplicate\",\"probability\":0.9}' ;;\n  esac\ndone\n";

#[cfg(unix)]
#[test]
fn judge_batch_reviews_two_pairs_and_passes_the_recipe() {
    let repo = temp_repo("reuse-judge-batch");
    // Two independent gray-band pairs: token_value (rejected by the stub) and
    // render_widget (kept).
    write(
        &repo,
        "a.py",
        "def normalize(value):\n    return value\n\ndef parse_token_value_data_extra(value):\n    return normalize(value)\n",
    );
    write(
        &repo,
        "b.py",
        "def normalize(value):\n    return value\n\ndef parse_token_value(value):\n    return normalize(value)\n",
    );
    write(
        &repo,
        "c.py",
        "def mount(value):\n    return value\n\ndef render_widget_extra(value):\n    return mount(value)\n",
    );
    write(
        &repo,
        "d.py",
        "def mount(value):\n    return value\n\ndef render_widget(value):\n    return mount(value)\n",
    );
    build(&repo);

    let args_file = repo.join("judge-args");
    let script = format!(
        "#!/bin/sh\nwhile IFS= read -r line; do\n  case \"$line\" in\n    *token_value*) printf '%s\\n' '{{\"decision\":\"distinct\",\"probability\":0.4}}' ;;\n    *) printf '%s\\n' '{{\"decision\":\"duplicate\",\"probability\":0.9}}' ;;\n  esac\ndone\nprintf '%s\\n' \"$*\" >> '{}'\n",
        args_file.display()
    );
    let bin = install_stub(&repo, &script);
    std::fs::write(repo.join("model/recipe.json"), "{\"threshold\":0.5}\n").unwrap();

    let baseline = eg(&repo).args(["reuse"]).output().unwrap();
    let baseline_out = String::from_utf8_lossy(&baseline.stdout);
    assert!(
        baseline_out.contains("2 duplication candidate(s) found"),
        "{baseline_out}"
    );

    let out = eg(&repo)
        .env("EG_JUDGE_BIN", &bin)
        .env("EG_JUDGE_MODEL", repo.join("model"))
        .args(["reuse", "--judge"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("1 duplication candidate(s) found"),
        "{stdout}"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("eg: judge: reviewed 2, rejected 1"),
        "{stderr}"
    );
    assert!(!stderr.contains("judge unavailable"), "{stderr}");

    // Recipe pass-through: the child argv carries --recipe with the model path.
    let args = std::fs::read_to_string(&args_file).unwrap();
    assert!(args.contains("--recipe"), "{args}");
    assert!(args.contains("recipe.json"), "{args}");

    // Structured journal counts land on the reuse --judge line.
    let log = std::fs::read_to_string(repo.join(".engram/usage.log")).unwrap();
    let last = log.lines().next_back().unwrap();
    let line: serde_json::Value = serde_json::from_str(last).unwrap();
    assert_eq!(line["tool"], "reuse");
    assert_eq!(line["judge"]["reviewed"], 2);
    assert_eq!(line["judge"]["rejected"], 1);
    assert_eq!(line["judge"]["failed"], 0);
    assert_eq!(line["judged"], true);
}

#[cfg(unix)]
#[test]
fn judge_noop_note_when_nothing_is_judgeable() {
    // When no candidate passes judge eligibility, the judge has nothing to
    // arbitrate and says so instead of staying silent.
    let repo = temp_repo("reuse-judge-noop");
    write(&repo, "a.py", "def alpha_one():\n    return 1\n");
    write(&repo, "b.py", "def beta_two():\n    return 2\n");
    build(&repo);
    let bin = install_stub(&repo, LINE_STUB);

    let out = eg(&repo)
        .env("EG_JUDGE_BIN", &bin)
        .env("EG_JUDGE_MODEL", repo.join("model"))
        .args(["reuse", "--judge"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("eg: judge: 0 judgeable candidates"),
        "{stderr}"
    );
    let log = std::fs::read_to_string(repo.join(".engram/usage.log")).unwrap();
    let last = log.lines().next_back().unwrap();
    let line: serde_json::Value = serde_json::from_str(last).unwrap();
    assert_eq!(line["judged"], false);
    assert_eq!(line["judge"]["reviewed"], 0);
}

/// The mem-write consolidation hint gains a semantic similarity figure only
/// when the caller opts in through EG_JUDGE_MEM_HINT=1.
#[cfg(unix)]
#[test]
fn mem_hint_appends_judge_similarity_when_opted_in() {
    use std::os::unix::fs::PermissionsExt;

    let repo = temp_repo("mem-judge-hint");
    let bin = repo.join("judge.sh");
    std::fs::write(
        &bin,
        "#!/bin/sh\necho '{\"decision\":\"duplicate\",\"similarity\":0.862,\"threshold\":0.106}'\n",
    )
    .unwrap();
    let mut perms = std::fs::metadata(&bin).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&bin, perms).unwrap();
    std::fs::create_dir_all(repo.join("model")).unwrap();
    std::fs::write(repo.join("model/lazy.json"), "{\"head_layers\":0}\n").unwrap();

    // Same token set, different prose: the 80% consolidation hint fires, but
    // the bodies are not content-hash duplicates.
    let base = "the resolver keeps confidence tiers; ambiguous edges keep dst_id null";
    let first = eg(&repo)
        .args(["mem", "write", "hints/a", "--body", base, "--kind", "fact"])
        .output()
        .unwrap();
    assert_eq!(
        first.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );

    let opted_in = eg(&repo)
        .env("EG_JUDGE_BIN", &bin)
        .env("EG_JUDGE_MODEL", repo.join("model"))
        .env("EG_JUDGE_MEM_HINT", "1")
        .args([
            "mem",
            "write",
            "hints/b",
            "--body",
            "the resolver keeps confidence tiers; ambiguous edges keep dst_id null the",
            "--kind",
            "fact",
        ])
        .output()
        .unwrap();
    assert_eq!(opted_in.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&opted_in.stdout);
    assert!(stdout.contains("eg: mem hint:"), "{stdout}");
    assert!(stdout.contains("(similarity=86%)"), "{stdout}");

    // Control: without the opt-in the hint keeps token-overlap wording only.
    let control = eg(&repo)
        .env("EG_JUDGE_BIN", &bin)
        .env("EG_JUDGE_MODEL", repo.join("model"))
        .args([
            "mem",
            "write",
            "hints/c",
            "--body",
            "null dst_id keep edges ambiguous tiers confidence keeps resolver the",
            "--kind",
            "fact",
        ])
        .output()
        .unwrap();
    assert_eq!(control.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&control.stdout);
    assert!(stdout.contains("eg: mem hint:"), "{stdout}");
    assert!(!stdout.contains("(similarity="), "{stdout}");
}
