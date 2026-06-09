use std::path::{Path, PathBuf};
use anyhow::{Result, Context};
use tracing::info;
use tokio::fs::{self, File};
use tokio::io::AsyncWriteExt;
use reqwest::Client;

pub struct ModelPaths {
    pub model_path: PathBuf,
    pub tokenizer_path: PathBuf,
}

pub async fn ensure_model_downloaded(data_dir: &str) -> Result<ModelPaths> {
    let models_dir = Path::new(data_dir).join("models");
    fs::create_dir_all(&models_dir).await?;

    let model_path = models_dir.join("all-MiniLM-L6-v2.safetensors");
    let tokenizer_path = models_dir.join("tokenizer.json");

    let client = Client::new();

    // Default HuggingFace links
    let model_url = std::env::var("EMBEDDING_MODEL_URL")
        .unwrap_or_else(|_| "https://huggingface.co/sentence-transformers/all-MiniLM-L6-v2/resolve/main/model.safetensors".to_string());
    
    let tokenizer_url = std::env::var("EMBEDDING_TOKENIZER_URL")
        .unwrap_or_else(|_| "https://huggingface.co/sentence-transformers/all-MiniLM-L6-v2/resolve/main/tokenizer.json".to_string());

    if !model_path.exists() {
        info!("Downloading embedding model weights from {}...", model_url);
        download_file(&client, &model_url, &model_path).await?;
    } else {
        info!("Local embedding model weights found: {:?}", model_path);
    }

    if !tokenizer_path.exists() {
        info!("Downloading tokenizer config from {}...", tokenizer_url);
        download_file(&client, &tokenizer_url, &tokenizer_path).await?;
    } else {
        info!("Local tokenizer config found: {:?}", tokenizer_path);
    }

    Ok(ModelPaths {
        model_path,
        tokenizer_path,
    })
}

async fn download_file(client: &Client, url: &str, dest: &Path) -> Result<()> {
    let response = client.get(url).send().await.context("Failed to send download request")?;
    if !response.status().is_success() {
        anyhow::bail!("Failed to download file from {}: HTTP {}", url, response.status());
    }

    let temp_dest = dest.with_extension("tmp");
    let mut file = File::create(&temp_dest).await.context("Failed to create temporary file")?;

    let mut stream = response.bytes_stream();
    use tokio_stream::StreamExt;
    
    while let Some(chunk) = stream.next().await {
        let bytes = chunk.context("Failed reading download chunk")?;
        file.write_all(&bytes).await.context("Failed writing chunk to file")?;
    }

    file.flush().await?;
    fs::rename(temp_dest, dest).await.context("Failed to rename temporary file to destination")?;
    info!("Download complete: {:?}", dest);
    Ok(())
}
