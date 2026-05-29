#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

//! Local embeddings for kungfu's semantic search.
//!
//! Design (locked in 2026-05-29):
//! - Engine: candle (pure Rust, no native deps).
//! - Model: BAAI/bge-small-en-v1.5 (384-dim, English-only, ~130MB).
//! - Storage: `~/.cache/kungfu/models/<model_id>/` (XDG-style, shared across projects).
//! - Default behaviour: opt-in only — `--semantic` flag or `[semantic] enabled = true` in
//!   `.kungfu/config.toml`. Without it, `semantic_search` falls back to query expansion.
//!
//! This crate currently ships the **scaffold**: types, store format, trait, no-op engine.
//! The real candle integration is gated behind the `inference` feature; deps are commented
//! out in `Cargo.toml` so default builds stay slim. Filling in the inference path requires:
//!   1. enable the commented-out candle/tokenizers/hf-hub deps in `Cargo.toml`;
//!   2. implement `CandleEngine::load` (download via hf-hub, init BertModel);
//!   3. implement `CandleEngine::embed_batch` (tokenize + forward + mean-pool + L2-normalize).
//!
//! Vector storage on disk: append-only `embeddings.bin` (raw f32 LE) plus a JSON
//! manifest mapping `symbol_id -> offset` and recording the model id + dim.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Embedding model identifier, used as the cache directory name.
pub const DEFAULT_MODEL_ID: &str = "BAAI/bge-small-en-v1.5";

/// Embedding dimensionality for the default model.
pub const DEFAULT_DIM: usize = 384;

/// Trait every concrete embedding backend implements.
pub trait EmbedEngine: Send + Sync {
    fn model_id(&self) -> &str;
    fn dim(&self) -> usize;

    /// Embed a batch of texts into row-major f32 vectors of length `dim`.
    fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>>;
}

/// On-disk manifest sitting next to `embeddings.bin`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbeddingManifest {
    pub model_id: String,
    pub dim: usize,
    /// `symbol_id -> offset (in vectors, not bytes)`.
    pub offsets: std::collections::BTreeMap<String, u64>,
    /// blake3 of the source text per id, so stale rows can be detected without re-embedding everything.
    pub digests: std::collections::BTreeMap<String, String>,
}

impl EmbeddingManifest {
    pub fn new(model_id: impl Into<String>, dim: usize) -> Self {
        Self {
            model_id: model_id.into(),
            dim,
            offsets: Default::default(),
            digests: Default::default(),
        }
    }

    pub fn manifest_path(index_dir: &Path) -> PathBuf {
        index_dir.join("embeddings.manifest.json")
    }

    pub fn data_path(index_dir: &Path) -> PathBuf {
        index_dir.join("embeddings.bin")
    }

    pub fn load(index_dir: &Path) -> Result<Option<Self>> {
        let p = Self::manifest_path(index_dir);
        if !p.exists() {
            return Ok(None);
        }
        let raw = std::fs::read(&p)?;
        Ok(Some(serde_json::from_slice(&raw)?))
    }

    pub fn save(&self, index_dir: &Path) -> Result<()> {
        let p = Self::manifest_path(index_dir);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_vec_pretty(self)?;
        std::fs::write(&p, json)?;
        Ok(())
    }
}

/// Hash the source text that an embedding represents.
pub fn text_digest(text: &str) -> String {
    blake3::hash(text.as_bytes()).to_hex().to_string()
}

/// Default cache root for model weights: `~/.cache/kungfu/models`.
pub fn default_models_dir() -> PathBuf {
    if let Some(home) = std::env::var_os("HOME") {
        PathBuf::from(home).join(".cache/kungfu/models")
    } else {
        PathBuf::from(".kungfu/models")
    }
}

/// Stub engine that returns an error on use. Lets the rest of the code wire to a real engine
/// later (behind the `inference` feature) without changing call-sites.
pub struct NoopEngine;

impl EmbedEngine for NoopEngine {
    fn model_id(&self) -> &str {
        DEFAULT_MODEL_ID
    }

    fn dim(&self) -> usize {
        DEFAULT_DIM
    }

    fn embed_batch(&self, _texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        anyhow::bail!(
            "kungfu was built without the `inference` feature; rebuild with \
             `cargo build --release --features semantic` once candle integration is enabled \
             (see kungfu-embed/src/lib.rs for the design)."
        )
    }
}

/// Try to construct the best available engine. Returns the noop if the real backend
/// isn't compiled in, so callers can always degrade gracefully.
pub fn open_default_engine() -> Box<dyn EmbedEngine> {
    #[cfg(feature = "inference")]
    {
        // TODO: replace with CandleEngine::load(default_models_dir(), DEFAULT_MODEL_ID).
        Box::new(NoopEngine)
    }
    #[cfg(not(feature = "inference"))]
    {
        Box::new(NoopEngine)
    }
}

/// L2-normalize a vector in place. Pulled into a shared helper so any backend can call it.
pub fn l2_normalize(v: &mut [f32]) {
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 1e-9 {
        for x in v.iter_mut() {
            *x /= norm;
        }
    }
}

/// Cosine similarity for two equal-length, ideally L2-normalized vectors. For normalized
/// vectors this is just the dot product; we compute it that way for speed.
pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    debug_assert_eq!(a.len(), b.len());
    a.iter().zip(b.iter()).map(|(x, y)| x * y).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_roundtrip() {
        let tmp = std::env::temp_dir().join(format!("kungfu_embed_test_{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();

        let mut m = EmbeddingManifest::new(DEFAULT_MODEL_ID, DEFAULT_DIM);
        m.offsets.insert("s:1".into(), 0);
        m.digests.insert("s:1".into(), text_digest("hello"));
        m.save(&tmp).unwrap();

        let loaded = EmbeddingManifest::load(&tmp).unwrap().unwrap();
        assert_eq!(loaded.model_id, DEFAULT_MODEL_ID);
        assert_eq!(loaded.dim, DEFAULT_DIM);
        assert_eq!(loaded.offsets["s:1"], 0);

        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn cosine_orthogonal_and_parallel() {
        let a = [1.0_f32, 0.0];
        let b = [0.0_f32, 1.0];
        assert!((cosine(&a, &b)).abs() < 1e-6);
        let c = [1.0_f32, 0.0];
        assert!((cosine(&a, &c) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn l2_normalize_unit() {
        let mut v = [3.0_f32, 4.0];
        l2_normalize(&mut v);
        let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-6);
    }

    #[test]
    fn noop_engine_errors_explicitly() {
        let e = NoopEngine;
        assert_eq!(e.dim(), DEFAULT_DIM);
        let err = e.embed_batch(&["x"]).unwrap_err().to_string();
        assert!(err.contains("inference"));
    }
}
