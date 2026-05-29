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
        let engine = open_default_engine();
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
            .map(|s| {
                let mut t = s.name.clone();
                if let Some(ref sig) = s.signature {
                    t.push(' ');
                    t.push_str(sig);
                }
                if let Some(ref doc) = s.doc_summary {
                    t.push(' ');
                    t.push_str(doc);
                }
                (s.id.clone(), t)
            })
            .collect();

        let total_known = texts.len();
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

        let batch_size = 32;
        let mut embedded = 0;
        for chunk in pending.chunks(batch_size) {
            let batch_texts: Vec<&str> = chunk.iter().map(|(_, t)| t.as_str()).collect();
            let vectors = engine.embed_batch(&batch_texts)?;
            for ((id, text), vec) in chunk.iter().zip(vectors.iter()) {
                append_vector(&index_dir, &mut manifest, id, text, vec)?;
            }
            embedded += chunk.len();
        }
        manifest.save(&index_dir)?;

        Ok(EmbeddingsBuildResult {
            embedded,
            total_in_store: manifest.offsets.len(),
            skipped_up_to_date: skipped,
        })
    }
}
