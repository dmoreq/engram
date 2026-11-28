use super::*;

#[test]
fn pure_classifier_precedence_matches_dashboard_memories() {
    let broken = |fk: RefFailureKind| ResolvedRef {
        status: RefStatus::Broken,
        kind: "path",
        detail: String::new(),
        mtime_secs: None,
        failure_kind: Some(fk),
    };
    let valid = || ResolvedRef {
        status: RefStatus::Valid,
        kind: "file",
        detail: "ok".to_string(),
        mtime_secs: Some(1),
        failure_kind: None,
    };

    assert_eq!(
        classify_note_pure("gotcha", "durable", "modified", &[("a.rs".into(), valid())]),
        Some(TriageReason::ModifiedAnchor)
    );
    assert_eq!(
        classify_note_pure(
            "gotcha",
            "durable",
            "broken",
            &[
                ("a.rs".into(), valid()),
                ("/x/y.rs".into(), broken(RefFailureKind::ExternalAbsPath)),
            ]
        ),
        Some(TriageReason::ExternalAbsPath)
    );
    assert_eq!(
        classify_note_pure(
            "gotcha",
            "durable",
            "broken",
            &[
                ("a.rs".into(), valid()),
                (".tmp/x.py".into(), broken(RefFailureKind::EphemeralTmpPath)),
            ]
        ),
        Some(TriageReason::EphemeralRefPartial)
    );
    assert_eq!(
        classify_note_pure(
            "gotcha",
            "durable",
            "broken",
            &[
                ("a.rs".into(), valid()),
                ("b.rs".into(), broken(RefFailureKind::MissingCodePath)),
            ]
        ),
        Some(TriageReason::PartialBrokenCode)
    );
    assert_eq!(
        classify_note_pure(
            "gotcha",
            "durable",
            "broken",
            &[(
                "docs/r.md".into(),
                broken(RefFailureKind::MissingDocOrReport),
            )]
        ),
        Some(TriageReason::DeadDurableNote)
    );
}

#[test]
fn json_truncates_whole_items_at_16kib_and_paginates_with_after_key() {
    let big_items: Vec<TriageItem> = (0..25)
        .map(|i| TriageItem {
            key: format!("note-{i:03}"),
            rev: 1,
            kind: "gotcha".into(),
            scope: "durable".into(),
            age_days: 5,
            recall_count: 0,
            ref_status: "broken",
            triage_reason: TriageReason::DeadDurableNote,
            triage_label: TriageReason::DeadDurableNote.label(),
            suggested_action: TriageAction::ReviewOrRetire,
            valid_refs: vec![],
            broken_refs: vec![BrokenRefEntry {
                token: format!("src/very/long/path/module_{i}_extra_padding_for_size.rs"),
                failure_kind: "missing_code_path",
            }],
            commit_context: Some(CommitContext {
                sha: "abcdef1".into(),
                subject: "x".repeat(180),
                sibling_files: vec!["src/a.rs".into(), "src/b.rs".into()],
            }),
            symbol_candidates: vec![],
        })
        .collect();
    let pkt = TriagePacket {
        schema_version: 1,
        untrusted_data: true,
        store: "local",
        index_state: IndexState::Ok,
        git_stats: GitStats::default(),
        total_candidates: 40,
        shown: 25,
        omitted_count: 15,
        next_after_key: None,
        truncated: true,
        items: big_items,
    };
    let out = render_triage_json(pkt);
    assert!(
        out.len() <= TRIAGE_MAX_PAYLOAD_BYTES,
        "rendered payload {} > {}",
        out.len(),
        TRIAGE_MAX_PAYLOAD_BYTES
    );
    let json_part = out.lines().skip(2).collect::<Vec<_>>().join("\n");
    let parsed: serde_json::Value = serde_json::from_str(&json_part).unwrap();
    assert_eq!(parsed["truncated"], true);
    let shown = parsed["shown"].as_u64().unwrap();
    let omitted = parsed["omitted_count"].as_u64().unwrap();
    assert_eq!(shown + omitted, 40);
    assert!(parsed["next_after_key"].as_str().is_some());
}

#[test]
fn utf8_subject_truncation_never_panics_on_oversized_first_item() {
    let utf8_subj = "đ".repeat(100);
    let huge_item = TriageItem {
        key: "note-utf8".into(),
        rev: 1,
        kind: "gotcha".into(),
        scope: "durable".into(),
        age_days: 1,
        recall_count: 0,
        ref_status: "broken",
        triage_reason: TriageReason::FileSplitSameCommit,
        triage_label: TriageReason::FileSplitSameCommit.label(),
        suggested_action: TriageAction::RelinkOrReview,
        valid_refs: vec![],
        broken_refs: (0..250)
            .map(|i| BrokenRefEntry {
                token: format!("src/long/path/item_{i:04}_padding_text.rs"),
                failure_kind: "missing_code_path",
            })
            .collect(),
        commit_context: Some(CommitContext {
            sha: "abcdef1".into(),
            subject: utf8_subj,
            sibling_files: vec!["a.rs".into(), "b.rs".into(), "c.rs".into()],
        }),
        symbol_candidates: vec![],
    };
    let pkt = TriagePacket {
        schema_version: 1,
        untrusted_data: true,
        store: "local",
        index_state: IndexState::Ok,
        git_stats: GitStats::default(),
        total_candidates: 1,
        shown: 1,
        omitted_count: 0,
        next_after_key: None,
        truncated: false,
        items: vec![huge_item],
    };
    let out = render_triage_json(pkt);
    assert!(out.contains("note-utf8"));
}
