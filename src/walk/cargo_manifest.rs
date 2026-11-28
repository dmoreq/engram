use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

thread_local! {
    static CURRENT_CRATE_NAME: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

pub(crate) fn take_current_crate_name() -> Option<String> {
    CURRENT_CRATE_NAME.with(|c| c.borrow_mut().take())
}

pub(super) fn set_current_crate_name_for(path: &Path) {
    let is_rs = path.extension().and_then(|e| e.to_str()) == Some("rs");
    let name = if is_rs {
        cargo_crate_name_for_file(path)
    } else {
        None
    };
    CURRENT_CRATE_NAME.with(|c| *c.borrow_mut() = name);
}

fn cargo_crate_name_for_file(path: &Path) -> Option<String> {
    let mut cur = path.parent()?;
    loop {
        let manifest = cur.join("Cargo.toml");
        if manifest.is_file() {
            if let Some(name) = cached_cargo_crate_name(&manifest) {
                return Some(name);
            }
        }
        if cur.join(".git").exists() {
            return None;
        }
        cur = cur.parent()?;
    }
}

fn cached_cargo_crate_name(manifest: &Path) -> Option<String> {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, Option<String>>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(v) = cache.lock().ok().and_then(|g| g.get(manifest).cloned()) {
        return v;
    }
    let parsed = parse_cargo_crate_name(&std::fs::read_to_string(manifest).ok()?);
    if let Ok(mut g) = cache.lock() {
        g.insert(manifest.to_path_buf(), parsed.clone());
    }
    parsed
}

pub(super) fn parse_cargo_crate_name(toml: &str) -> Option<String> {
    let (mut section, mut pkg_name, mut lib_name) = ("", None, None);
    for raw_line in toml.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(rest) = line.strip_prefix('[') {
            section = match rest.split(']').next().unwrap_or("").trim() {
                "package" => "package",
                "lib" => "lib",
                _ => "other",
            };
            continue;
        }
        if !matches!(section, "package" | "lib") {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        if k.trim() != "name" {
            continue;
        }
        let val = v.trim();
        let Some(quote) = val.chars().next().filter(|&c| c == '"' || c == '\'') else {
            continue;
        };
        if let Some(end) = val[1..].find(quote) {
            let norm = val[1..1 + end].replace('-', "_");
            if section == "lib" {
                lib_name = Some(norm);
            } else if pkg_name.is_none() {
                pkg_name = Some(norm);
            }
        }
    }
    lib_name.or(pkg_name)
}
