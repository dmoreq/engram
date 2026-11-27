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

/// The configured text checkpoint, when the env var points at a directory
/// that carries a checkpoint manifest. Unset/invalid -> None (embeddings off).
pub fn text_model_dir() -> Option<PathBuf> {
    let value = std::env::var("EG_JUDGE_TEXT_MODEL").ok()?;
    if value.is_empty() {
        return None;
    }
    let dir = PathBuf::from(value);
    dir.join("lazy.json").is_file().then_some(dir)
}

/// Embed one text through `eg-judge embed`. `None` on any failure: a missing
/// binary, a timeout, or a malformed payload must never fail the note write.
pub fn embed_text(text: &str) -> Option<Vec<f32>> {
    let model = text_model_dir()?;
    let binary = judge_proc::find_bin()?;
    let home = std::env::var("HOME").ok();
    let mut command = judge_proc::base_command(&binary, home.as_deref());
    command
        .arg("embed")
        .arg("--text")
        .arg(text)
        .arg("--model")
        .arg(&model);
    let raw = judge_proc::run_bounded(command, None, EMBED_TIMEOUT, 256 * 1024)?;
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
        // No env var -> None regardless of the environment.
        if std::env::var("EG_JUDGE_TEXT_MODEL").is_ok_and(|v| !v.is_empty()) {
            return; // a real model is configured on this host; skip the absence case
        }
        assert!(text_model_dir().is_none());
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
