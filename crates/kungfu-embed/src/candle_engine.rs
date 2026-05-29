//! Candle-based BERT embedding engine.
//!
//! Loads BAAI/bge-small-en-v1.5 (or any compatible Bert-family model) from a local
//! `models_dir/<model_id>/` directory containing `config.json`, `tokenizer.json`,
//! and `model.safetensors`. Uses CPU by default — bge-small inference is fast enough
//! on CPU that we don't bother with CUDA/Metal probing in v1.

use crate::{l2_normalize, EmbedEngine, DEFAULT_DIM};
use anyhow::{Context, Result};
use candle_core::{DType, Device, Tensor};
use candle_nn::VarBuilder;
use candle_transformers::models::bert::{BertModel, Config, DTYPE};
use std::path::{Path, PathBuf};
use tokenizers::{PaddingParams, PaddingStrategy, Tokenizer, TruncationParams, TruncationStrategy};

pub struct CandleEngine {
    model: BertModel,
    tokenizer: Tokenizer,
    device: Device,
    model_id: String,
    dim: usize,
    max_seq_len: usize,
}

impl CandleEngine {
    /// Load a Bert-family model from `models_dir/<sanitised model_id>/`. The directory
    /// must contain `config.json`, `tokenizer.json`, and `model.safetensors` — exactly
    /// what `hf-hub` writes after `kungfu embeddings install`.
    pub fn load(models_dir: &Path, model_id: &str) -> Result<Self> {
        let dir = models_dir.join(sanitize_model_id(model_id));
        anyhow::ensure!(
            dir.is_dir(),
            "model directory not found: {} (run `kungfu embeddings install`)",
            dir.display()
        );

        let config_path = dir.join("config.json");
        let tokenizer_path = dir.join("tokenizer.json");
        let weights_path = dir.join("model.safetensors");

        let config_raw = std::fs::read_to_string(&config_path)
            .with_context(|| format!("read {}", config_path.display()))?;
        let config: Config = serde_json::from_str(&config_raw)
            .with_context(|| format!("parse {}", config_path.display()))?;

        let mut tokenizer = Tokenizer::from_file(&tokenizer_path)
            .map_err(|e| anyhow::anyhow!("load tokenizer: {}", e))?;
        // Force batch-friendly padding/truncation so encode_batch yields tensors
        // of uniform length.
        tokenizer.with_padding(Some(PaddingParams {
            strategy: PaddingStrategy::BatchLongest,
            ..Default::default()
        }));
        tokenizer
            .with_truncation(Some(TruncationParams {
                max_length: config.max_position_embeddings.min(512),
                strategy: TruncationStrategy::LongestFirst,
                ..Default::default()
            }))
            .map_err(|e| anyhow::anyhow!("set truncation: {}", e))?;

        let device = Device::Cpu;
        let vb = unsafe {
            VarBuilder::from_mmaped_safetensors(&[weights_path.as_path()], DTYPE, &device)?
        };
        let model = BertModel::load(vb, &config)?;

        Ok(Self {
            model,
            tokenizer,
            device,
            model_id: model_id.to_string(),
            dim: config.hidden_size,
            max_seq_len: config.max_position_embeddings.min(512),
        })
    }
}

impl EmbedEngine for CandleEngine {
    fn model_id(&self) -> &str {
        &self.model_id
    }

    fn dim(&self) -> usize {
        self.dim
    }

    fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }

        let encodings = self
            .tokenizer
            .encode_batch(texts.to_vec(), true)
            .map_err(|e| anyhow::anyhow!("tokenize batch: {}", e))?;

        let batch = encodings.len();
        let seq = encodings.first().map(|e| e.len()).unwrap_or(0);
        anyhow::ensure!(seq > 0, "tokenizer returned empty encodings");
        anyhow::ensure!(
            seq <= self.max_seq_len,
            "encoded length {} exceeds model max {}",
            seq,
            self.max_seq_len
        );

        // Flatten into row-major Vec<u32> for Tensor::from_vec.
        let mut ids: Vec<u32> = Vec::with_capacity(batch * seq);
        let mut mask: Vec<u32> = Vec::with_capacity(batch * seq);
        let types: Vec<u32> = vec![0; batch * seq];
        for enc in &encodings {
            ids.extend_from_slice(enc.get_ids());
            mask.extend_from_slice(enc.get_attention_mask());
        }

        let input_ids = Tensor::from_vec(ids, (batch, seq), &self.device)?;
        let attention_mask = Tensor::from_vec(mask, (batch, seq), &self.device)?;
        let token_type_ids = Tensor::from_vec(types, (batch, seq), &self.device)?;

        let hidden = self
            .model
            .forward(&input_ids, &token_type_ids, Some(&attention_mask))?;
        // hidden: (batch, seq, hidden_size)

        // Mean-pool with mask: sum(hidden * mask) / sum(mask)
        let mask_f = attention_mask.to_dtype(DType::F32)?;
        let mask_expanded = mask_f.unsqueeze(2)?; // (batch, seq, 1)
        let masked = hidden.broadcast_mul(&mask_expanded)?;
        let summed = masked.sum(1)?; // (batch, hidden)
        let counts = mask_expanded.sum(1)?; // (batch, 1)
        let pooled = summed.broadcast_div(&counts)?;

        let mut rows: Vec<Vec<f32>> = pooled.to_vec2()?;
        for row in &mut rows {
            l2_normalize(row);
        }
        Ok(rows)
    }
}

/// Replace `/` with `--` so HuggingFace ids like `BAAI/bge-small-en-v1.5` survive
/// as a single filesystem directory name.
pub fn sanitize_model_id(model_id: &str) -> String {
    model_id.replace('/', "--")
}

/// Default download paths used by both the CLI installer and the engine loader.
pub fn model_dir_for(models_dir: &Path, model_id: &str) -> PathBuf {
    models_dir.join(sanitize_model_id(model_id))
}

/// Sanity check: spec'd dim matches what the model reports (helps catch a wrong model).
pub fn assert_dim_matches(engine: &CandleEngine) -> Result<()> {
    anyhow::ensure!(
        engine.dim() == DEFAULT_DIM,
        "loaded model has dim={} but kungfu expected {}",
        engine.dim(),
        DEFAULT_DIM
    );
    Ok(())
}
