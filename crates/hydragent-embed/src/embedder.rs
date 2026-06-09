use anyhow::{Result, Context};
use std::path::Path;
use candle_core::{Device, Tensor};
use candle_transformers::models::bert::{BertModel, Config as BertConfig};
use tokenizers::Tokenizer;

pub struct LocalEmbedder {
    model: BertModel,
    tokenizer: Tokenizer,
    device: Device,
}

impl LocalEmbedder {
    pub fn new(model_path: &Path, tokenizer_path: &Path) -> Result<Self> {
        let device = Device::Cpu;
        
        let tokenizer = Tokenizer::from_file(tokenizer_path)
            .map_err(|e| anyhow::anyhow!("Failed to load tokenizer: {}", e))?;

        let config = BertConfig {
            vocab_size: 30522,
            hidden_size: 384,
            num_hidden_layers: 6,
            num_attention_heads: 12,
            intermediate_size: 1536,
            hidden_act: candle_transformers::models::bert::HiddenAct::Gelu,
            hidden_dropout_prob: 0.1,
            max_position_embeddings: 512,
            type_vocab_size: 2,
            initializer_range: 0.02,
            layer_norm_eps: 1e-12,
            pad_token_id: 0,
            position_embedding_type: candle_transformers::models::bert::PositionEmbeddingType::Absolute,
            use_cache: true,
            classifier_dropout: None,
            model_type: None,
        };

        let vb = unsafe {
            candle_nn::VarBuilder::from_mmaped_safetensors(&[model_path], candle_core::DType::F32, &device)
                .context("Failed loading safetensors")?
        };

        let model = BertModel::load(vb, &config).context("Failed creating BertModel")?;

        Ok(Self {
            model,
            tokenizer,
            device,
        })
    }

    pub fn embed_text(&self, text: &str) -> Result<Vec<f32>> {
        let tokens = self.tokenizer.encode(text, true)
            .map_err(|e| anyhow::anyhow!("Tokenization failed: {}", e))?;

        let input_ids = tokens.get_ids();
        let token_type_ids = tokens.get_type_ids();
        
        let input_ids_tensor = Tensor::new(input_ids, &self.device)?.unsqueeze(0)?;
        let token_type_ids_tensor = Tensor::new(token_type_ids, &self.device)?.unsqueeze(0)?;

        let ys = self.model.forward(&input_ids_tensor, &token_type_ids_tensor, None)?;

        let attention_mask = tokens.get_attention_mask();
        let attention_mask_tensor = Tensor::new(attention_mask, &self.device)?.unsqueeze(0)?;

        let embeddings = mean_pooling(&ys, &attention_mask_tensor)?;
        
        let normalized = l2_normalize(&embeddings)?;
        
        let vec = normalized.squeeze(0)?.to_vec1::<f32>()?;
        Ok(vec)
    }
}

fn mean_pooling(token_embeddings: &Tensor, attention_mask: &Tensor) -> Result<Tensor> {
    let (_n_batch, _n_seq, _hidden_size) = token_embeddings.dims3()?;
    
    let attention_mask_expanded = attention_mask.unsqueeze(2)?
        .to_dtype(candle_core::DType::F32)?
        .broadcast_as(token_embeddings.shape())?;
    
    let masked_embeddings = token_embeddings.to_dtype(candle_core::DType::F32)?
        .mul(&attention_mask_expanded)?;
    
    let sum_embeddings = masked_embeddings.sum(1)?;
    let sum_mask = attention_mask_expanded.sum(1)?;
    let clamped_mask = sum_mask.clamp(1e-9, f32::MAX)?;
    
    let mean = sum_embeddings.div(&clamped_mask.broadcast_as(sum_embeddings.shape())?)?;
    Ok(mean)
}

fn l2_normalize(tensor: &Tensor) -> Result<Tensor> {
    let square_sum = tensor.sqr()?.sum_keepdim(1)?;
    let norm = square_sum.sqrt()?;
    let clamped_norm = norm.clamp(1e-12, f32::MAX)?;
    let normalized = tensor.broadcast_div(&clamped_norm)?;
    Ok(normalized)
}

pub fn cosine_similarity(v1: &[f32], v2: &[f32]) -> f32 {
    if v1.len() != v2.len() || v1.is_empty() {
        return 0.0;
    }
    let mut dot_product = 0.0;
    let mut norm_v1 = 0.0;
    let mut norm_v2 = 0.0;
    for i in 0..v1.len() {
        dot_product += v1[i] * v2[i];
        norm_v1 += v1[i] * v1[i];
        norm_v2 += v2[i] * v2[i];
    }
    if norm_v1 == 0.0 || norm_v2 == 0.0 {
        return 0.0;
    }
    dot_product / (norm_v1.sqrt() * norm_v2.sqrt())
}
