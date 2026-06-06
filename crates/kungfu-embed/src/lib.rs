#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

//! Local embeddings for kungfu's semantic search.
//!
//! - Engine: candle (pure Rust, no native deps).
//! - Model: BAAI/bge-small-en-v1.5 (384-dim, English-only, ~130MB).
//! - Storage: `~/.cache/kungfu/models/<model_id>/` (XDG-style, shared across projects).
//! - Default behaviour: opt-in. The CLI must be built with `--features semantic` for the
//!   `inference` flag to flow into this crate; otherwise `open_default_engine` returns the
//!   `NoopEngine` and `semantic_search` falls back to query expansion.
//!
//! Vector store on disk:
//! - `embeddings.bin` — raw little-endian f32, row-major, `n_vectors * dim` floats.
//! - `embeddings.manifest.json` — `model_id`, `dim`, `symbol_id -> row offset`,
//!   `symbol_id -> blake3(text)` so we only re-embed changed symbols on rebuild.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[cfg(feature = "inference")]
pub mod candle_engine;
#[cfg(feature = "inference")]
pub use candle_engine::CandleEngine;

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

    /// `true` when this engine can actually produce embeddings. `NoopEngine` returns
    /// `false` so callers can distinguish "feature not compiled / model missing" from
    /// "real engine ready" without trying a probe call.
    fn is_real(&self) -> bool {
        true
    }
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

    fn is_real(&self) -> bool {
        false
    }
}

/// Process-wide cached engine for the latency-sensitive read paths (`ask_context`,
/// `semantic_search`). The default model is ~130MB on disk and rebuilding the in-memory
/// `BertModel` is expensive; without this cache every request reloaded it from scratch,
/// which pegged CPU and stalled the MCP server. The model is immutable
/// (`EmbedEngine: Send + Sync`), so a single shared instance is safe to share by reference.
///
/// The engine is resolved once and cached for the life of the process. If it could not be
/// built at first use (feature off / weights missing) it stays `NoopEngine` until restart,
/// so read paths degrade to keyword search. The explicit `embeddings_build` /
/// `embeddings_status` paths deliberately keep using `open_default_engine` so a freshly
/// installed model is picked up without a restart.
pub fn shared_engine() -> &'static dyn EmbedEngine {
    static ENGINE: std::sync::OnceLock<Box<dyn EmbedEngine>> = std::sync::OnceLock::new();
    ENGINE.get_or_init(open_default_engine).as_ref()
}

/// Try to construct the best available engine. With the `inference` feature compiled in
/// and the model installed at `default_models_dir()`, returns a real `CandleEngine`;
/// otherwise (or on any load error) falls back to `NoopEngine` so callers can degrade
/// gracefully to query expansion.
pub fn open_default_engine() -> Box<dyn EmbedEngine> {
    #[cfg(feature = "inference")]
    {
        match CandleEngine::load(&default_models_dir(), DEFAULT_MODEL_ID) {
            Ok(e) => return Box::new(e),
            Err(e) => {
                tracing::debug!("CandleEngine load failed, falling back to noop: {}", e);
            }
        }
    }
    Box::new(NoopEngine)
}

/// Download model weights from HuggingFace into `models_dir/<sanitised id>/`.
/// Pulls `config.json`, `tokenizer.json`, and `model.safetensors`. Skips files
/// already present (idempotent). Bypasses hf-hub due to its broken redirect
/// handling on 0.3.2 — uses ureq directly with manual Location following.
#[cfg(feature = "inference")]
pub fn install_model(models_dir: &Path, model_id: &str) -> Result<PathBuf> {
    use anyhow::Context;
    let target = candle_engine::model_dir_for(models_dir, model_id);
    std::fs::create_dir_all(&target).with_context(|| format!("mkdir {}", target.display()))?;

    for fname in ["config.json", "tokenizer.json", "model.safetensors"] {
        let dest = target.join(fname);
        if dest.exists()
            && std::fs::metadata(&dest)
                .map(|m| m.len() > 0)
                .unwrap_or(false)
        {
            continue;
        }
        let url = format!("https://huggingface.co/{}/resolve/main/{}", model_id, fname);
        download(&url, &dest).with_context(|| format!("download {}", fname))?;
    }
    Ok(target)
}

#[cfg(feature = "inference")]
fn download(url: &str, dest: &Path) -> Result<()> {
    use std::io::{Read, Write};

    let mut current = url.to_string();
    // Follow up to 5 redirects manually; HF serves 307s to its CDN.
    for _ in 0..5 {
        let resp = ureq::get(&current)
            .call()
            .map_err(|e| anyhow::anyhow!("{}", e))?;
        let status = resp.status();
        if (300..400).contains(&status) {
            if let Some(loc) = resp.header("Location") {
                current = if loc.starts_with("http") {
                    loc.to_string()
                } else if let Some(host_end) = current
                    .find('/')
                    .and_then(|i| current[i + 2..].find('/').map(|j| j + i + 2))
                {
                    format!("{}{}", &current[..host_end], loc)
                } else {
                    loc.to_string()
                };
                continue;
            }
            anyhow::bail!("redirect with no Location at {}", current);
        }
        anyhow::ensure!(
            status == 200,
            "unexpected status {} for {}",
            status,
            current
        );
        let mut reader = resp.into_reader();
        let mut buf = Vec::new();
        reader.read_to_end(&mut buf)?;
        let tmp = dest.with_extension("download");
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(&buf)?;
        f.sync_all()?;
        std::fs::rename(&tmp, dest)?;
        return Ok(());
    }
    anyhow::bail!("too many redirects starting at {}", url)
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

/// Read-side view over `embeddings.bin` + `embeddings.manifest.json`. Vectors are stored
/// row-major as raw little-endian f32; `manifest.offsets` maps `symbol_id` to row index
/// (multiply by `dim * 4` for the byte offset).
pub struct EmbeddingStore {
    manifest: EmbeddingManifest,
    data: Vec<f32>,
}

impl EmbeddingStore {
    pub fn load(index_dir: &Path) -> Result<Option<Self>> {
        let manifest = match EmbeddingManifest::load(index_dir)? {
            Some(m) => m,
            None => return Ok(None),
        };
        let data_path = EmbeddingManifest::data_path(index_dir);
        let raw = std::fs::read(&data_path)?;
        anyhow::ensure!(raw.len() % 4 == 0, "embeddings.bin not aligned to f32");
        let floats: Vec<f32> = raw
            .chunks_exact(4)
            .map(|c| {
                let bytes = [c[0], c[1], c[2], c[3]];
                f32::from_le_bytes(bytes)
            })
            .collect();
        anyhow::ensure!(
            floats.len().is_multiple_of(manifest.dim),
            "embeddings.bin length {} not divisible by dim {}",
            floats.len(),
            manifest.dim
        );
        Ok(Some(Self {
            manifest,
            data: floats,
        }))
    }

    pub fn manifest(&self) -> &EmbeddingManifest {
        &self.manifest
    }

    pub fn dim(&self) -> usize {
        self.manifest.dim
    }

    pub fn len(&self) -> usize {
        self.data.len() / self.manifest.dim.max(1)
    }

    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    pub fn vector(&self, row: u64) -> Option<&[f32]> {
        let dim = self.manifest.dim;
        let start = (row as usize) * dim;
        if start + dim > self.data.len() {
            return None;
        }
        Some(&self.data[start..start + dim])
    }

    /// Top-K symbol IDs by cosine similarity to `query`. Both query and stored vectors
    /// are expected to be L2-normalised — cosine reduces to dot product.
    pub fn top_k(&self, query: &[f32], k: usize) -> Vec<(String, f32)> {
        debug_assert_eq!(query.len(), self.manifest.dim);
        let mut scored: Vec<(&String, f32)> = self
            .manifest
            .offsets
            .iter()
            .filter_map(|(id, row)| self.vector(*row).map(|v| (id, cosine(query, v))))
            .collect();
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        scored
            .into_iter()
            .take(k)
            .map(|(id, s)| (id.clone(), s))
            .collect()
    }
}

/// Append a vector to `embeddings.bin` and update the manifest. Used by the build path.
pub fn append_vector(
    index_dir: &Path,
    manifest: &mut EmbeddingManifest,
    symbol_id: &str,
    text: &str,
    vector: &[f32],
) -> Result<()> {
    anyhow::ensure!(
        vector.len() == manifest.dim,
        "vector dim {} != manifest dim {}",
        vector.len(),
        manifest.dim
    );
    let data_path = EmbeddingManifest::data_path(index_dir);
    if let Some(parent) = data_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // Compute next row offset based on current file size, not manifest.offsets.len(),
    // so a partial write isn't double-counted on resume.
    let current_size = std::fs::metadata(&data_path).map(|m| m.len()).unwrap_or(0);
    let row = current_size / (manifest.dim as u64 * 4);

    let mut bytes: Vec<u8> = Vec::with_capacity(vector.len() * 4);
    for v in vector {
        bytes.extend_from_slice(&v.to_le_bytes());
    }
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&data_path)?;
    f.write_all(&bytes)?;

    manifest.offsets.insert(symbol_id.to_string(), row);
    manifest
        .digests
        .insert(symbol_id.to_string(), text_digest(text));
    Ok(())
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
