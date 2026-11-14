//! Asset text guards: dashboard UI affordances pinned against app.js.
//!
//! No JS harness exists in the repo, so the behavioral contract (I-13, I-15)
//! is fenced here by reading the shipped asset text; the manual pass — click a
//! Memory Store row selects the store-id option, a second click returns to
//! All, expanded rows survive one SSE cycle, Tab/Enter activate rows and the
//! label target — stays the documented check.

const APP_JS: &str = include_str!("../../../src/dashboard/assets/app.js");

/// Slice the body of a top-level `function name(...) {...}` in app.js.
/// Top-level declarations start at column 0, so the next `\nfunction ` bound
/// ends the body; a missing name fails loudly instead of passing vacuously.
fn fn_body(name: &str) -> String {
    let needle = format!("function {name}(");
    let start = APP_JS
        .find(&needle)
        .unwrap_or_else(|| panic!("app.js: function {name} not found"));
    let rest = &APP_JS[start..];
    let end = rest
        .bytes()
        .position(|b| b == b'\n')
        .and_then(|nl| rest[nl + 1..].find("\nfunction ").map(|i| nl + 1 + i))
        .unwrap_or(rest.len());
    rest[..end].to_string()
}

#[test]
fn dashboard_asset_memory_store_filter_matches_option_and_never_appends() {
    let body = fn_body("filterMemoryByRepo");
    // Rows pass the display name while options carry store_id values, so the
    // match must accept either side and select the matched option value.
    assert!(
        body.contains("opt.value === repoName || opt.textContent === repoName"),
        "filterMemoryByRepo must match the option by value or text: {body}"
    );
    assert!(
        body.contains("select.value === matchedValue"),
        "the toggle must compare the matched option value, not the raw label: {body}"
    );
    assert!(
        body.contains("select.value = 'ALL'"),
        "an unmatched name must fall back to ALL: {body}"
    );
    // No ghost option: the handler must never fabricate a missing entry.
    assert!(
        !body.contains("appendChild"),
        "filterMemoryByRepo must not append options: {body}"
    );
    assert!(
        body.contains("renderMemoryList()"),
        "every path re-renders: {body}"
    );
}

#[test]
fn dashboard_asset_worktree_expansion_survives_renders() {
    assert!(
        APP_JS.contains("const expandedWorktrees = new Set()"),
        "the expansion set must exist at module level"
    );
    let body = fn_body("renderReposTable");
    // The set is read while rebuilding rows (aria-expanded + wt-open), not
    // only written by the toggle — that read is what survives SSE renders.
    assert!(
        body.contains("expandedWorktrees.has(label)"),
        "renderReposTable must re-apply persisted expansion: {body}"
    );
    assert!(
        body.contains("wt-open"),
        "open rows re-apply the wt-open class: {body}"
    );
    assert!(
        body.contains("expandedWorktrees.add(label)")
            && body.contains("expandedWorktrees.delete(label)"),
        "the toggle must persist expansion changes: {body}"
    );
}

#[test]
fn dashboard_asset_agent_tables_keyboard_accessible() {
    for name in ["renderReuseAgentsTable", "renderMemoryAgentsTable"] {
        let body = fn_body(name);
        assert!(
            body.contains("tr.tabIndex = 0"),
            "{name}: rows must be focusable: {body}"
        );
        assert!(
            body.contains("onkeydown"),
            "{name}: rows must handle keydown: {body}"
        );
        assert!(
            body.contains("preventDefault()"),
            "{name}: Enter/Space must preventDefault: {body}"
        );
    }
}

#[test]
fn dashboard_asset_filter_rows_drop_aria_label_and_announce() {
    // Screen readers read the cells; the removed `<tr role="row">` labels must
    // stay gone, and every filter handler must announce the action instead.
    for name in [
        "filterByRepo",
        "filterByAgent",
        "filterReuseByRepo",
        "filterMemoryByRepo",
    ] {
        let body = fn_body(name);
        assert!(
            body.contains("announce('Filtered by"),
            "{name} must announce the filter action: {body}"
        );
    }
    assert!(
        !APP_JS.contains("aria-label', 'Filter by")
            && !APP_JS.contains("aria-label', 'Toggle worktrees"),
        "filter rows must not carry a row-level aria-label"
    );
}

#[test]
fn dashboard_asset_clickable_row_class_stays_absent() {
    assert_eq!(
        APP_JS.matches("clickable-row").count(),
        0,
        "the removed clickable-row class must not return"
    );
}
