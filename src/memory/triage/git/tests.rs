use super::*;
use crate::db::test_support::{make_dir_all, remove_tree, temp_root, write_file};
use crate::memory::triage::{BrokenRefEntry, TriageAction};

fn git_cmd(root: &Path, args: &[&str]) {
    let st = Command::new("git")
        .current_dir(root)
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(st.success(), "git {:?} failed", args);
}

#[test]
fn git_detects_rename_1_to_1_file_split_same_dir_and_doc_move() {
    let root = temp_root("triage-git");
    make_dir_all(root.join("src/ui")).unwrap();
    make_dir_all(root.join("docs")).unwrap();
    write_file(
        root.join("src/ui/app.js"),
        b"// line 1\n// line 2\n// line 3\n",
    )
    .unwrap();
    write_file(root.join("docs/old.md"), b"# Old Guide\nContent line\n").unwrap();

    git_cmd(&root, &["init"]);
    git_cmd(&root, &["config", "user.name", "Test"]);
    git_cmd(&root, &["config", "user.email", "test@example.com"]);
    git_cmd(&root, &["add", "."]);
    git_cmd(&root, &["commit", "-m", "init"]);

    git_cmd(&root, &["mv", "docs/old.md", "docs/new.md"]);
    git_cmd(&root, &["rm", "src/ui/app.js"]);
    make_dir_all(root.join("src/ui")).unwrap();
    write_file(root.join("src/ui/core.js"), b"// core\n").unwrap();
    write_file(root.join("src/ui/mem.js"), b"// mem\n").unwrap();
    git_cmd(&root, &["add", "."]);
    git_cmd(&root, &["commit", "-m", "split ui and move doc"]);

    let mut items = vec![
        TriageItem {
            key: "k-doc".into(),
            rev: 1,
            kind: "fact".into(),
            scope: "durable".into(),
            age_days: 1,
            recall_count: 0,
            ref_status: "broken",
            triage_reason: TriageReason::DeadDurableNote,
            triage_label: TriageReason::DeadDurableNote.label(),
            suggested_action: TriageAction::ReviewOrRetire,
            valid_refs: vec![],
            broken_refs: vec![BrokenRefEntry {
                token: "docs/old.md".into(),
                failure_kind: "missing_doc_or_report",
            }],
            commit_context: None,
            symbol_candidates: vec![],
        },
        TriageItem {
            key: "k-split".into(),
            rev: 1,
            kind: "gotcha".into(),
            scope: "durable".into(),
            age_days: 1,
            recall_count: 0,
            ref_status: "broken",
            triage_reason: TriageReason::DeadDurableNote,
            triage_label: TriageReason::DeadDurableNote.label(),
            suggested_action: TriageAction::ReviewOrRetire,
            valid_refs: vec![],
            broken_refs: vec![BrokenRefEntry {
                token: "src/ui/app.js".into(),
                failure_kind: "missing_code_path",
            }],
            commit_context: None,
            symbol_candidates: vec![],
        },
    ];

    let stats = enrich_with_git(Some(&root), &mut items, false, GitBudget::default());
    assert!(!stats.disabled);
    assert_eq!(items[0].triage_reason, TriageReason::Renamed1To1);
    assert_eq!(
        items[0].commit_context.as_ref().unwrap().sibling_files,
        vec!["docs/new.md"]
    );
    assert_eq!(items[1].triage_reason, TriageReason::FileSplitSameCommit);
    assert_eq!(
        items[1].commit_context.as_ref().unwrap().sibling_files,
        vec!["src/ui/core.js", "src/ui/mem.js"]
    );
    remove_tree(&root);
}

#[test]
fn git_caps_at_10_refs_and_handles_non_git_and_kill_switch() {
    let root = temp_root("triage-nongit");
    let mut items = vec![];
    let stats = enrich_with_git(Some(&root), &mut items, false, GitBudget::default());
    assert!(stats.disabled);
    remove_tree(&root);

    let repo = temp_root("triage-cap");
    make_dir_all(repo.join("src")).unwrap();
    write_file(repo.join("src/a.rs"), b"fn a() {}\n").unwrap();
    git_cmd(&repo, &["init"]);
    git_cmd(&repo, &["config", "user.name", "Test"]);
    git_cmd(&repo, &["config", "user.email", "test@example.com"]);
    git_cmd(&repo, &["add", "."]);
    git_cmd(&repo, &["commit", "-m", "init\x1b[31m"]);

    git_cmd(&repo, &["mv", "src/a.rs", "src/b.rs"]);

    let mut cap_items: Vec<TriageItem> = (0..12)
        .map(|i| TriageItem {
            key: format!("k-{i:02}"),
            rev: 1,
            kind: "gotcha".into(),
            scope: "durable".into(),
            age_days: 1,
            recall_count: 0,
            ref_status: "broken",
            triage_reason: TriageReason::DeadDurableNote,
            triage_label: TriageReason::DeadDurableNote.label(),
            suggested_action: TriageAction::ReviewOrRetire,
            valid_refs: vec![],
            broken_refs: vec![BrokenRefEntry {
                token: if i == 0 {
                    "src/a.rs".into()
                } else {
                    format!("src/missing_{i}.rs")
                },
                failure_kind: "missing_code_path",
            }],
            commit_context: None,
            symbol_candidates: vec![],
        })
        .collect();

    let s_nogit = enrich_with_git(Some(&repo), &mut cap_items, true, GitBudget::default());
    assert!(s_nogit.disabled);

    let s_cap = enrich_with_git(Some(&repo), &mut cap_items, false, GitBudget::default());
    assert!(!s_cap.disabled);
    assert_eq!(cap_items[0].triage_reason, TriageReason::UncommittedMove);
    assert_eq!(s_cap.capped, 1);
    remove_tree(&repo);
}
