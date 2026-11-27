//! Opt-in semantic judge hint for the mem-write consolidation hint. The
//! eg-judge invocation lives beside the guard so the write path stays
//! advisory and write-confinement untouched.

use super::guard::ConsolidationMatch;
use super::judge_proc;

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
        let binary = judge_proc::find_bin()?;
        let model = judge_proc::resolve_model_dir().ok()?;
        let home = std::env::var("HOME").ok();
        let mut command = judge_proc::base_command(&binary, home.as_deref());
        command
            .arg("similarity")
            .arg("--text-a")
            .arg(&old_body)
            .arg("--text-b")
            .arg(&note.body)
            .arg("--model")
            .arg(model);
        let raw = judge_proc::run_bounded(
            command,
            None,
            std::time::Duration::from_millis(15_000),
            65_536,
        )?;
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

/// Percent (0-100) from one `eg-judge similarity` JSON line.
fn parse_similarity_pct(raw: &str) -> Option<u64> {
    let value: serde_json::Value = serde_json::from_str(raw.trim()).ok()?;
    let similarity = value.get("similarity")?.as_f64()?;
    if !similarity.is_finite() || !(0.0..=1.0).contains(&similarity) {
        return None;
    }
    Some((similarity * 100.0).round() as u64)
}

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
