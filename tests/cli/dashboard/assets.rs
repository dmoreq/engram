//! Asset text guards: dashboard UI affordances pinned against app.js.
//!
//! No JS harness exists in the repo, so the behavioral contract (I-13, I-15)
//! is fenced here by reading the shipped asset text; the manual pass — click a
//! Memory Store row selects the store-id option, a second click returns to
//! All, expanded rows survive one SSE cycle, Tab/Enter activate rows and the
//! label target — stays the documented check.

const APP_JS: &str = concat!(
    include_str!("../../../src/dashboard/assets/core.js"),
    "\n",
    include_str!("../../../src/dashboard/assets/overview.js"),
    "\n",
    include_str!("../../../src/dashboard/assets/telemetry.js"),
    "\n",
    include_str!("../../../src/dashboard/assets/reuse.js"),
    "\n",
    include_str!("../../../src/dashboard/assets/memory.js"),
);

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
    // match must accept either side and delegate toggle/reset to setMemoryStoreFilter.
    assert!(
        body.contains("opt.value === repoName || opt.textContent === repoName"),
        "filterMemoryByRepo must match the option by value or text: {body}"
    );
    assert!(
        body.contains("setMemoryStoreFilter("),
        "filterMemoryByRepo must delegate to setMemoryStoreFilter: {body}"
    );
    // No ghost option: the handler must never fabricate a missing entry.
    assert!(
        !body.contains("appendChild"),
        "filterMemoryByRepo must not append options: {body}"
    );

    let set_body = fn_body("setMemoryStoreFilter");
    assert!(
        set_body.contains("select.value === storeId"),
        "the toggle must compare the matched storeId value: {set_body}"
    );
    assert!(
        set_body.contains("? 'ALL' : storeId"),
        "re-clicking the active store must fall back to ALL: {set_body}"
    );
    assert!(
        set_body.contains("state.memories.selectedId = null"),
        "store filter change must reset selectedId: {set_body}"
    );
    assert!(
        set_body.contains("renderMemoryList()"),
        "every path re-renders: {set_body}"
    );
}

#[test]
fn dashboard_asset_unified_agent_table_keyboard_accessible() {
    // The three per-tab agent tables collapsed into one unified attribution
    // table; its rows keep the keyboard contract.
    let body = fn_body("renderAgentsTable");
    assert!(
        body.contains("tr.tabIndex = 0"),
        "rows must be focusable: {body}"
    );
    assert!(
        body.contains("onkeydown"),
        "rows must handle keydown: {body}"
    );
    assert!(
        body.contains("preventDefault()"),
        "Enter/Space must preventDefault: {body}"
    );
}

#[test]
fn dashboard_asset_filter_rows_drop_aria_label_and_announce() {
    // Screen readers read the cells; the removed `<tr role="row">` labels must
    // stay gone, and every filter handler must announce the action instead.
    for name in [
        "filterByRepo",
        "filterByAgent",
        "filterReuseByRepo",
        "setMemoryStoreFilter",
    ] {
        let body = fn_body(name);
        assert!(
            body.contains("announce("),
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
