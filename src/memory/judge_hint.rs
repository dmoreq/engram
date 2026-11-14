//! Opt-in semantic judge hint for the mem-write consolidation hint. The
//! eg-judge invocation lives beside the guard so the write path stays
//! advisory and write-confinement untouched.

use super::guard::ConsolidationMatch;

impl ConsolidationMatch {
    /// Opt-in semantic similarity via the local `eg-judge` binary
    /// (`EG_JUDGE_MEM_HINT=1`). Advisory only: any failure — missing binary
    /// or checkpoint, timeout, malformed output — returns `None` and the hint
    /// keeps its token-overlap wording. Never blocks or alters the write.
    pub fn semantic_similarity_pct(
        &self,
        conn: &rusqlite::Connection,
        note: &crate::memory::NormalizedNote,
    ) -> Option<u64> {
        if std::env::var("EG_JUDGE_MEM_HINT").as_deref() != Ok("1") {
            return None;
        }
        let old_body = existing_body(conn, &self.key, self.rev)?;
        if old_body.trim().is_empty() || note.body.trim().is_empty() {
            return None;
        }
        let (binary, model) = judge_setup()?;
        let mut command = std::process::Command::new(binary);
        command
            .arg("similarity")
            .arg("--text-a")
            .arg(&old_body)
            .arg("--text-b")
            .arg(&note.body)
            .arg("--model")
            .arg(model)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .env_clear();
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        let mut child = command.spawn().ok()?;
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(15_000);
        loop {
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                _ => {
                    kill_judge_group(&child);
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
            }
        }
        kill_judge_group(&child);
        let mut raw = String::new();
        use std::io::Read;
        let _ = child.stdout.take()?.take(65_537).read_to_string(&mut raw);
        parse_similarity_pct(&raw)
    }
}

fn existing_body(conn: &rusqlite::Connection, key: &str, rev: i64) -> Option<String> {
    conn.query_row(
        "SELECT body FROM memories WHERE key = ?1 AND rev = ?2 LIMIT 1",
        rusqlite::params![key, rev],
        |row| row.get::<_, String>(0),
    )
    .ok()
}

/// `EG_JUDGE_BIN` wins, then `PATH`; the model defaults to the shared
/// `~/.engram/judge/model` checkpoint and must contain `lazy.json`.
fn judge_setup() -> Option<(std::path::PathBuf, std::path::PathBuf)> {
    let binary = match std::env::var("EG_JUDGE_BIN") {
        Ok(value) if !value.is_empty() => std::path::PathBuf::from(value),
        _ => {
            let paths = std::env::var_os("PATH")?;
            std::env::split_paths(&paths)
                .map(|dir| dir.join("eg-judge"))
                .find(|candidate| candidate.is_file())?
        }
    };
    let model = match std::env::var("EG_JUDGE_MODEL") {
        Ok(value) if !value.is_empty() => std::path::PathBuf::from(value),
        _ => std::path::PathBuf::from(std::env::var_os("HOME")?)
            .join(".engram")
            .join("judge")
            .join("model"),
    };
    model.join("lazy.json").is_file().then_some((binary, model))
}

/// Percent (0-100) from one `eg-judge similarity` JSON line.
fn parse_similarity_pct(raw: &str) -> Option<u64> {
    let value: serde_json::Value = serde_json::from_str(raw.trim()).ok()?;
    let similarity = value.get("similarity")?.as_f64()?;
    if !similarity.is_finite() || !(0.0..=1.0).contains(&similarity) {
        return None;
    }
    Some((similarity * 100.0).round() as u64)
}

#[cfg(unix)]
fn kill_judge_group(child: &std::process::Child) {
    let group = format!("-{}", child.id());
    for signal in ["-TERM", "-KILL"] {
        let _ = std::process::Command::new("kill")
            .arg(signal)
            .arg(&group)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }
}

#[cfg(not(unix))]
fn kill_judge_group(_child: &std::process::Child) {}

#[cfg(test)]
mod hint_tests {
    use super::*;

    #[test]
    fn similarity_pct_parses_one_json_line() {
        assert_eq!(
            parse_similarity_pct(
                r#"{"decision":"duplicate","similarity":0.409,"threshold":0.106}"#
            ),
            Some(41)
        );
        assert_eq!(parse_similarity_pct("not json"), None);
        assert_eq!(
            parse_similarity_pct(r#"{"decision":"duplicate","similarity":1.5}"#),
            None
        );
    }
}
