use crate::parse::SOURCE_EXTENSIONS;

/// Test classification (schema v8).
pub(crate) fn is_test_path(rel: &str) -> bool {
    let lower = rel.to_ascii_lowercase();
    let file = lower.rsplit('/').next().unwrap_or(&lower);
    let dir_part = &lower[..lower.len() - file.len()];
    let dir_hit = dir_part
        .split('/')
        .any(|seg| matches!(seg, "tests" | "test" | "__tests__"));
    let file_hit = (file.starts_with("test_") && file.ends_with(".py"))
        || file.ends_with("_test.py")
        || file.ends_with("_test.go")
        || file.ends_with("_test.dart")
        || file.ends_with("_test.rs")
        || file.ends_with("_tests.rs");
    let dotted_hit = lower.contains(".test.") || lower.contains(".spec.");
    dir_hit || file_hit || dotted_hit
}

pub(crate) fn is_test_name(name: &str) -> bool {
    name.to_ascii_lowercase().starts_with("test_")
        || (name.starts_with("test") && name.chars().nth(4).is_some_and(|c| c.is_ascii_uppercase()))
}

/// Derive the dot-joined module path from the repo-relative file path.
/// Package/root markers (`__init__.py`, `index.*` TS/JS files, `mod.rs`,
/// `lib.rs`) collapse into the containing module path.
pub fn module_path_for(rel: &str) -> (String, bool) {
    let file_name = rel.rsplit('/').next().unwrap_or(rel);
    let is_py_pkg = file_name == "__init__.py";
    let is_ts_pkg = SOURCE_EXTENSIONS
        .iter()
        .any(|(ext, lang)| lang.is_ts_js() && file_name.strip_suffix(ext) == Some("index"));
    let is_rust_root = file_name == "mod.rs" || file_name == "lib.rs";
    if is_py_pkg || is_ts_pkg || is_rust_root {
        let dir = rel
            .strip_suffix(file_name)
            .unwrap_or(rel)
            .trim_end_matches('/')
            .replace('/', ".");
        if dir.is_empty() {
            if is_rust_root {
                return (file_name.trim_end_matches(".rs").to_string(), true);
            }
            return ("root".to_string(), true);
        }
        return (dir, true);
    }
    let stem = SOURCE_EXTENSIONS
        .iter()
        .find_map(|(ext, _)| rel.strip_suffix(ext))
        .unwrap_or(rel);
    if stem.is_empty() {
        return ("root".to_string(), false);
    }
    (stem.replace('/', "."), false)
}

pub(super) fn rust_crate_root(rel: &str) -> Option<String> {
    let file_name = rel.rsplit('/').next().unwrap_or(rel);
    match rel.rfind("/src/") {
        Some(i) => Some(format!("{}.src", rel[..i].replace('/', "."))),
        None if rel.starts_with("src/") => Some("src".to_string()),
        None if file_name == "lib.rs" || file_name == "main.rs" => Some(String::new()),
        None => None,
    }
}

pub(super) fn rust_lib_root(rel: &str) -> Option<String> {
    if let Some(root) = rust_crate_root(rel) {
        return Some(root);
    }
    if let Some(i) = rel.rfind("/tests/") {
        return Some(format!("{}.src", rel[..i].replace('/', ".")));
    }
    if rel.starts_with("tests/") {
        return Some("src".to_string());
    }
    None
}
