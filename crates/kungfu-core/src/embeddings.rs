use crate::KungfuService;
use anyhow::Result;
use kungfu_embed::{
    append_vector, default_models_dir, open_default_engine, text_digest, EmbeddingManifest,
    DEFAULT_DIM, DEFAULT_MODEL_ID,
};
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct EmbeddingsStatus {
    pub model_id: String,
    pub dim: usize,
    pub inference_compiled: bool,
    pub weights_installed: bool,
    pub index_present: bool,
    pub indexed_vectors: usize,
    pub indexed_symbols: usize,
    pub models_dir: String,
    /// One-line hint for the agent on what to do next, if anything.
    pub hint: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct EmbeddingsBuildResult {
    pub embedded: usize,
    pub total_in_store: usize,
    pub skipped_up_to_date: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct EmbeddingsSyncResult {
    /// "synced" | "up_to_date" | "no_store" (never built — sync won't auto-opt-in)
    /// | "no_engine" (feature off or weights missing — sync won't auto-download)
    pub status: String,
    pub embedded: usize,
    pub pruned: usize,
    pub compacted: bool,
}

/// The text a symbol is embedded as: name + signature + doc summary.
fn symbol_embed_text(s: &kungfu_types::symbol::Symbol) -> String {
    let mut t = s.name.clone();
    if let Some(ref sig) = s.signature {
        t.push(' ');
        t.push_str(sig);
    }
    if let Some(ref doc) = s.doc_summary {
        t.push(' ');
        t.push_str(doc);
    }
    t
}

impl KungfuService {
    /// Report whether semantic vector search is wired up end-to-end and, if not, which
    /// step is missing. Always succeeds — designed so agents can ask "are we ready?"
    /// without crashing on partial setups.
    pub fn embeddings_status(&self) -> Result<EmbeddingsStatus> {
        let engine = open_default_engine();
        let inference_compiled = engine.is_real();

        let models_dir = default_models_dir();
        let weights_dir = models_dir.join(DEFAULT_MODEL_ID.replace('/', "--"));
        let weights_installed = weights_dir.join("model.safetensors").exists();

        let index_dir = self.project.index_dir();
        let manifest = EmbeddingManifest::load(&index_dir).ok().flatten();
        let index_present = manifest.is_some();
        let indexed_vectors = manifest.as_ref().map(|m| m.offsets.len()).unwrap_or(0);

        let indexed_symbols = self.store.load_symbols().map(|s| s.len()).unwrap_or(0);

        let hint = if !inference_compiled {
            "binary built without `--features semantic`; rebuild to enable vector search"
                .to_string()
        } else if !weights_installed {
            "weights not installed; run `kungfu embeddings install` (~130MB)".to_string()
        } else if !index_present || indexed_vectors == 0 {
            "vectors not built; run `kungfu embeddings build` after `kungfu index`".to_string()
        } else if indexed_vectors < indexed_symbols {
            format!(
                "{}/{} symbols embedded; rerun `kungfu embeddings build` to catch up",
                indexed_vectors, indexed_symbols
            )
        } else {
            "ready — semantic_search will use vector top-K".to_string()
        };

        Ok(EmbeddingsStatus {
            model_id: engine.model_id().to_string(),
            dim: engine.dim(),
            inference_compiled,
            weights_installed,
            index_present,
            indexed_vectors,
            indexed_symbols,
            models_dir: models_dir.to_string_lossy().to_string(),
            hint,
        })
    }

    /// Build or refresh embeddings for every indexed symbol. Skips symbols whose
    /// `name+signature+doc` text hash already matches the manifest.
    ///
    /// Requires a real engine (`--features semantic` + installed model). Returns a clear
    /// error otherwise instead of silently doing nothing.
    pub fn embeddings_build(&self) -> Result<EmbeddingsBuildResult> {
        self.ensure_fresh_index()?;
        let mut engine = open_default_engine();
        if !engine.is_real() {
            // One explicit command should yield working vectors: if the binary has
            // inference compiled in but weights are missing, download them now.
            // Without the feature, install_model errors with the rebuild hint.
            kungfu_embed::install_model(&default_models_dir(), DEFAULT_MODEL_ID)?;
            engine = open_default_engine();
        }
        anyhow::ensure!(
            engine.is_real(),
            "no real embedding engine available — check `embeddings_status` for the missing step"
        );

        let symbols = self.store.load_symbols()?;
        anyhow::ensure!(
            !symbols.is_empty(),
            "no symbols indexed — run `kungfu index` first"
        );

        let index_dir = self.project.index_dir();
        let mut manifest = EmbeddingManifest::load(&index_dir)?
            .unwrap_or_else(|| EmbeddingManifest::new(engine.model_id(), DEFAULT_DIM));
        anyhow::ensure!(
            manifest.dim == engine.dim(),
            "existing manifest dim {} != engine dim {}; rebuild from scratch",
            manifest.dim,
            engine.dim()
        );

        let texts: Vec<(String, String)> = symbols
            .iter()
            .map(|s| (s.id.clone(), symbol_embed_text(s)))
            .collect();

        let total_known = texts.len();
        let live_ids: std::collections::HashSet<String> =
            texts.iter().map(|(id, _)| id.clone()).collect();
        let pending: Vec<(String, String)> = texts
            .into_iter()
            .filter(|(id, text)| {
                manifest
                    .digests
                    .get(id)
                    .map(|d| *d != text_digest(text))
                    .unwrap_or(true)
            })
            .collect();
        let skipped = total_known - pending.len();

        // Drop vectors for symbols that no longer exist — reindexes change symbol ids
        // (they embed the file content hash), so without pruning the store grows
        // unboundedly and top-K wastes slots on dead ids.
        kungfu_embed::prune_and_compact(&index_dir, &mut manifest, &live_ids)?;

        let embedded = embed_pending(&index_dir, engine.as_ref(), &mut manifest, &pending)?;

        Ok(EmbeddingsBuildResult {
            embedded,
            total_in_store: manifest.offsets.len(),
            skipped_up_to_date: skipped,
        })
    }

    /// Incrementally bring the vector store in line with the current index: prune
    /// vectors for symbols that no longer exist, embed new/changed ones. Designed
    /// for background use after a reindex — it never auto-opts-in (no manifest →
    /// no-op) and never auto-downloads weights; both stay explicit via
    /// `embeddings_build`. The cheap digest diff runs before any model load.
    pub fn embeddings_sync(&self) -> Result<EmbeddingsSyncResult> {
        let index_dir = self.project.index_dir();
        let mut manifest = match EmbeddingManifest::load(&index_dir)? {
            Some(m) => m,
            None => {
                return Ok(EmbeddingsSyncResult {
                    status: "no_store".into(),
                    embedded: 0,
                    pruned: 0,
                    compacted: false,
                })
            }
        };

        let symbols = self.store.load_symbols()?;
        let texts: Vec<(String, String)> = symbols
            .iter()
            .map(|s| (s.id.clone(), symbol_embed_text(s)))
            .collect();
        let live_ids: std::collections::HashSet<String> =
            texts.iter().map(|(id, _)| id.clone()).collect();
        let pending: Vec<(String, String)> = texts
            .into_iter()
            .filter(|(id, text)| {
                manifest
                    .digests
                    .get(id)
                    .map(|d| *d != text_digest(text))
                    .unwrap_or(true)
            })
            .collect();

        let (pruned, compacted) =
            kungfu_embed::prune_and_compact(&index_dir, &mut manifest, &live_ids)?;

        if pending.is_empty() {
            return Ok(EmbeddingsSyncResult {
                status: "up_to_date".into(),
                embedded: 0,
                pruned,
                compacted,
            });
        }

        let engine = open_default_engine();
        if !engine.is_real() {
            return Ok(EmbeddingsSyncResult {
                status: "no_engine".into(),
                embedded: 0,
                pruned,
                compacted,
            });
        }

        let embedded = embed_pending(&index_dir, engine.as_ref(), &mut manifest, &pending)?;
        Ok(EmbeddingsSyncResult {
            status: "synced".into(),
            embedded,
            pruned,
            compacted,
        })
    }
}

/// Embed `pending` (id, text) pairs in batches, appending to the store. The manifest
/// is flushed periodically so a crash mid-build doesn't discard finished batches —
/// `embeddings.bin` is append-only and the manifest is the only row↔symbol link.
fn embed_pending(
    index_dir: &std::path::Path,
    engine: &dyn kungfu_embed::EmbedEngine,
    manifest: &mut EmbeddingManifest,
    pending: &[(String, String)],
) -> Result<usize> {
    let batch_size = 32;
    let flush_every = 16;
    let mut embedded = 0;
    for (batch_idx, chunk) in pending.chunks(batch_size).enumerate() {
        let batch_texts: Vec<&str> = chunk.iter().map(|(_, t)| t.as_str()).collect();
        let vectors = engine.embed_batch(&batch_texts)?;
        for ((id, text), vec) in chunk.iter().zip(vectors.iter()) {
            append_vector(index_dir, manifest, id, text, vec)?;
        }
        embedded += chunk.len();
        if (batch_idx + 1) % flush_every == 0 {
            manifest.save(index_dir)?;
        }
    }
    manifest.save(index_dir)?;
    Ok(embedded)
}
