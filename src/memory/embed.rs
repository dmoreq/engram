//! Optional note-embedding plumbing. The text model is a separate eg-judge
//! checkpoint (`EG_JUDGE_TEXT_MODEL`, absent from the shipped distribution):
//! everything here degrades to `None` when the model or binary is missing, so
//! ranking falls back to the lexical features and the embedding column stays
//! NULL. Inference runs through the local `eg-judge` binary — zero outbound
//! network, no Python runtime, child lifetime bound to one call.

use std::path::PathBuf;
use std::time::Duration;

use serde_json::Value;

use super::judge_proc;

/// One cold Metal start plus one warm embed, matching the judge deadline.
const EMBED_TIMEOUT: Duration = Duration::from_millis(20_000);
const BATCH_MAX_INPUT: usize = 256 * 1024;
const BATCH_MAX_OUTPUT: usize = 4 * 1024 * 1024;

fn is_encoder_only_manifest(dir: &std::path::Path) -> bool {
    let Ok(raw) = std::fs::read_to_string(dir.join("lazy.json")) else {
        return false;
    };
    serde_json::from_str::<Value>(&raw)
        .ok()
        .and_then(|v| v.get("head_layers").and_then(Value::as_u64))
        == Some(0)
}

/// The configured text checkpoint: `EG_JUDGE_TEXT_MODEL`, else
/// `~/.engram/judge/text-model`, else `~/.engram/judge/model` when its
/// `lazy.json` declares `head_layers == 0`. Unset/invalid -> None.
pub fn text_model_dir() -> Option<PathBuf> {
    text_model_dir_with(
        std::env::var("EG_JUDGE_TEXT_MODEL").ok().as_deref(),
        std::env::var("HOME").ok().as_deref(),
    )
}

fn text_model_dir_with(env_val: Option<&str>, home: Option<&str>) -> Option<PathBuf> {
    if let Some(value) = env_val {
        if !value.is_empty() {
            let dir = PathBuf::from(value);
            return dir.join("lazy.json").is_file().then_some(dir);
        }
    }
    let home = home?;
    let judge_dir = PathBuf::from(home).join(".engram").join("judge");
    let text_model = judge_dir.join("text-model");
    if text_model.join("lazy.json").is_file() {
        return Some(text_model);
    }
    let model = judge_dir.join("model");
    if model.join("lazy.json").is_file() && is_encoder_only_manifest(&model) {
        return Some(model);
    }
    None
}

fn parse_embedding_line(raw: &str) -> Option<Vec<f32>> {
    let value: Value = serde_json::from_str(raw.trim()).ok()?;
    let dim = value.get("dim")?.as_u64()? as usize;
    let vector: Vec<f32> = value
        .get("embedding")?
        .as_array()?
        .iter()
        .map(|v| v.as_f64().map(|n| n as f32))
        .collect::<Option<_>>()?;
    (vector.len() == dim && dim > 0).then_some(vector)
}

/// Embed one text through `eg-judge embed` (or the persistent coprocess when
/// enabled). `None` on any failure: a missing binary, a timeout, or a
/// malformed payload must never fail the note write.
pub fn embed_text(text: &str) -> Option<Vec<f32>> {
    let model = text_model_dir()?;
    let binary = judge_proc::find_bin()?;
    let home = std::env::var("HOME").ok();
    if judge_proc::is_coprocess_enabled() {
        if let Ok(req) = serde_json::to_string(&serde_json::json!({"text": text})) {
            if let Some(raw) =
                judge_proc::serve_request(&binary, &model, home.as_deref(), &req, EMBED_TIMEOUT)
            {
                if let Some(vec) = parse_embedding_line(&raw) {
                    return Some(vec);
                }
            }
        }
    }
    let mut command = judge_proc::base_command(&binary, home.as_deref());
    command
        .arg("embed")
        .arg("--text")
        .arg(text)
        .arg("--model")
        .arg(&model);
    let raw = judge_proc::run_bounded(command, None, EMBED_TIMEOUT, 256 * 1024)?;
    parse_embedding_line(&raw)
}

/// Embed a slice of texts in batched NDJSON `eg-judge embed --stdin` calls,
/// avoiding `O(N)` cold Metal reloads during `mem_reindex --embed`.
pub fn embed_batch(texts: &[&str]) -> crate::error::Result<Vec<Option<Vec<f32>>>> {
    if texts.is_empty() {
        return Ok(Vec::new());
    }
    let Some(model) = text_model_dir() else {
        return Ok(vec![None; texts.len()]);
    };
    let Some(binary) = judge_proc::find_bin() else {
        return Ok(vec![None; texts.len()]);
    };
    let home = std::env::var("HOME").ok();
    let mut out = Vec::with_capacity(texts.len());
    let mut idx = 0;
    while idx < texts.len() {
        let mut payload = Vec::new();
        let chunk_start = idx;
        while idx < texts.len() {
            let line =
                serde_json::to_vec(&serde_json::json!({"text": texts[idx]})).unwrap_or_default();
            if idx > chunk_start && payload.len() + line.len() + 1 > BATCH_MAX_INPUT {
                break;
            }
            payload.extend_from_slice(&line);
            payload.push(b'\n');
            idx += 1;
        }
        let chunk_len = idx - chunk_start;
        let mut command = judge_proc::base_command(&binary, home.as_deref());
        command
            .arg("embed")
            .arg("--stdin")
            .arg("--model")
            .arg(&model);
        let Some(raw) =
            judge_proc::run_bounded(command, Some(&payload), EMBED_TIMEOUT, BATCH_MAX_OUTPUT)
        else {
            out.extend(std::iter::repeat_n(None, chunk_len));
            continue;
        };
        let mut lines = raw.lines().map(str::trim).filter(|l| !l.is_empty());
        for _ in 0..chunk_len {
            out.push(lines.next().and_then(parse_embedding_line));
        }
    }
    Ok(out)
}

/// L2-normalised f32 vector -> little-endian BLOB bytes.
pub fn embedding_blob(vector: &[f32]) -> Vec<u8> {
    vector.iter().flat_map(|v| v.to_le_bytes()).collect()
}

/// BLOB bytes -> f32 vector; `None` on a non-multiple-of-4 length.
pub fn embedding_from_blob(bytes: &[u8]) -> Option<Vec<f32>> {
    if bytes.is_empty() || !bytes.len().is_multiple_of(4) {
        return None;
    }
    // Constant-size chunks read as fixed arrays (`as_chunks`, Rust 1.88+).
    let (chunks, _rest) = bytes.as_chunks::<4>();
    Some(chunks.iter().map(|c| f32::from_le_bytes(*c)).collect())
}

/// Cosine of two L2-normalised vectors is a dot product; clamp to [-1, 1].
pub fn cosine(left: &[f32], right: &[f32]) -> f32 {
    if left.len() != right.len() {
        return 0.0;
    }
    let dot: f32 = left.iter().zip(right).map(|(a, b)| a * b).sum();
    dot.clamp(-1.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_model_dir_requires_a_manifest() {
        let root = crate::db::test_support::temp_root("embed-model-missing");
        assert!(text_model_dir_with(None, root.to_str()).is_none());
        crate::db::test_support::remove_tree(&root);
    }

    #[test]
    fn text_model_dir_falls_back_to_text_model_and_zero_head_model() {
        let root = crate::db::test_support::temp_root("embed-model-fallback");
        let model_dir = root.join(".engram/judge/model");
        crate::db::test_support::make_dir_all(&model_dir).unwrap();
        crate::db::test_support::write_file(model_dir.join("lazy.json"), b"{\"head_layers\":1}")
            .unwrap();
        // head_layers != 0 is rejected as a text model fallback.
        assert!(text_model_dir_with(None, root.to_str()).is_none());

        // head_layers == 0 is accepted as fallback.
        crate::db::test_support::write_file(model_dir.join("lazy.json"), b"{\"head_layers\":0}")
            .unwrap();
        assert_eq!(
            text_model_dir_with(None, root.to_str()).as_deref(),
            Some(model_dir.as_path())
        );

        // text-model takes precedence over model.
        let text_dir = root.join(".engram/judge/text-model");
        crate::db::test_support::make_dir_all(&text_dir).unwrap();
        crate::db::test_support::write_file(text_dir.join("lazy.json"), b"{}").unwrap();
        assert_eq!(
            text_model_dir_with(None, root.to_str()).as_deref(),
            Some(text_dir.as_path())
        );

        crate::db::test_support::remove_tree(&root);
    }

    #[test]
    fn embed_batch_empty_returns_empty() {
        assert!(embed_batch(&[]).unwrap().is_empty());
    }

    #[test]
    fn blob_round_trip_preserves_values() {
        let vector = vec![0.25f32, -0.5, 1.0, 0.0];
        let bytes = embedding_blob(&vector);
        assert_eq!(bytes.len(), 16);
        assert_eq!(embedding_from_blob(&bytes), Some(vector));
    }

    #[test]
    fn odd_blob_length_is_rejected() {
        assert_eq!(embedding_from_blob(&[0, 0, 0]), None);
        assert_eq!(embedding_from_blob(&[]), None);
    }

    #[test]
    fn cosine_of_normalized_vectors_is_a_dot_product() {
        let v = vec![0.6f32, 0.8];
        assert!((cosine(&v, &v) - 1.0).abs() < 1e-6);
        assert!((cosine(&v, &[0.8, -0.6]) - 0.0).abs() < 1e-6);
    }

    #[test]
    fn cosine_length_mismatch_is_zero() {
        assert_eq!(cosine(&[1.0], &[1.0, 2.0]), 0.0);
    }
}
