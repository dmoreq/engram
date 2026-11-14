//! Packaging guards: the crate include list ships the fixture submodules and
//! docs (CUJ 2), and the doc faces state the real CLI/MCP contracts (I-17).

use std::fs;
use std::path::Path;

/// Mirror of the absolute-path leak patterns the hygiene gate enforces
/// (tests/scripts/hygiene_check.sh, `LEAK_PATTERNS`): the macOS home prefix,
/// the Linux home prefix with a user segment, and the workspace root. The
/// literals are split at compile time because this guard lives in a scanned
/// `.rs` file itself. Secret patterns are out of scope here; the baseline is
/// redacted to `<repo-root>` before it is ever tracked.
fn contains_leak_path(text: &str) -> bool {
    const USERS: &str = concat!("/Us", "ers/");
    const WORKSPACE: &str = concat!("/Work", "space/");
    const HOME: &str = concat!("/ho", "me/");
    if text.contains(USERS) || text.contains(WORKSPACE) {
        return true;
    }
    let mut rest = text;
    while let Some(idx) = rest.find(HOME) {
        let after = &rest[idx + HOME.len()..];
        let seg = after.split('/').next().unwrap_or("");
        if !seg.is_empty()
            && seg
                .chars()
                .all(|c| c.is_alphanumeric() || c == '.' || c == '_' || c == '-')
        {
            return true;
        }
        rest = after;
    }
    false
}

/// CUJ 2: the include list must cover the fixture submodules (`main.rs` declares
/// `mod common; mod misc; ...`) and the top-level docs (`documented_eg_commands_parse`
/// reads `docs/` at depth 1), and must refuse the internal review baseline. In a
/// source checkout the relocated baseline is tracked, so its redaction is asserted;
/// inside a packaged crate the directory is absent by construction (the include
/// list refuses `/docs/baselines/**`) and this walk stays a no-op.
#[test]
fn cargo_toml_include_covers_fixtures_submodules_and_docs() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let toml = fs::read_to_string(root.join("Cargo.toml")).unwrap();
    assert!(
        toml.contains("\"/tests/fixtures/*.rs\"") || toml.contains("\"/tests/fixtures/**\""),
        "include must cover the fixture submodules, not only main.rs"
    );
    assert!(
        toml.contains("\"/docs/*.md\""),
        "include must ship top-level docs"
    );
    assert!(
        !toml.contains("/docs/baselines"),
        "the internal review baseline must not ship in the crate package"
    );
    if let Ok(entries) = fs::read_dir(root.join("docs/baselines")) {
        for entry in entries {
            let path = entry.unwrap().path();
            if path.extension().and_then(|e| e.to_str()) != Some("md") {
                continue;
            }
            let text = fs::read_to_string(&path).unwrap();
            assert!(
                !contains_leak_path(&text),
                "workstation path leak in the tracked baseline: {}",
                path.display()
            );
        }
    }
}

/// I-17: the health automation prompt must use the real `mem write` contract
/// (`<KEY>` is a positional argument; no `--key` flag exists), and the
/// `--all-tools` count in docs/cli-reference.md must equal `TOOL_SPECS`.
#[test]
fn health_doc_and_cli_reference_match_cli_and_mcp_contracts() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let health = fs::read_to_string(root.join("docs/health.md")).unwrap();
    assert!(
        !health.contains("--key "),
        "health.md passes a --key flag that `eg mem write` does not define"
    );
    let cli_ref = fs::read_to_string(root.join("docs/cli-reference.md")).unwrap();
    assert!(
        cli_ref.contains(&format!(
            "Exposes all {} tools",
            engram::service::TOOL_SPECS.len()
        )),
        "cli-reference --all-tools count drifted from TOOL_SPECS"
    );
}
