pub mod model_downloader;
pub mod embedder;

pub use model_downloader::{ensure_model_downloaded, ModelPaths};
pub use embedder::{LocalEmbedder, cosine_similarity};

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_similarity_bounds() {
        let test_data_dir = "../../target/test_data";
        let paths = ensure_model_downloaded(test_data_dir).await.unwrap();
        
        let embedder = LocalEmbedder::new(&paths.model_path, &paths.tokenizer_path).unwrap();

        let s1 = "My cat is sleeping";
        let s2 = "A feline is napping";
        
        let v1 = embedder.embed_text(s1).unwrap();
        let v2 = embedder.embed_text(s2).unwrap();
        
        let sim1 = cosine_similarity(&v1, &v2);
        println!("Similarity (similar): {}", sim1);
        assert!(sim1 > 0.65, "Expected similarity > 0.65, got {}", sim1);

        let s3 = "The stock market crashed";
        let v3 = embedder.embed_text(s3).unwrap();
        
        let sim2 = cosine_similarity(&v1, &v3);
        println!("Similarity (unrelated): {}", sim2);
        assert!(sim2 < 0.4, "Expected similarity < 0.4, got {}", sim2);
    }
}
