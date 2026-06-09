use serde::{Serialize, Deserialize};
use std::path::Path;
use anyhow::Result;
use std::collections::HashMap;

#[derive(Serialize, Deserialize, Default)]
pub struct VectorStore {
    embeddings: HashMap<String, Vec<f32>>,
}

impl VectorStore {
    pub fn new() -> Self {
        Self {
            embeddings: HashMap::new(),
        }
    }

    pub fn insert(&mut self, id: String, vector: Vec<f32>) {
        self.embeddings.insert(id, vector);
    }

    pub fn search(&self, query_vec: &[f32], k: usize) -> Vec<(String, f32)> {
        let mut results = Vec::new();
        for (id, vec) in &self.embeddings {
            let sim = hydragent_embed::cosine_similarity(query_vec, vec);
            results.push((id.clone(), sim));
        }

        results.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        results.truncate(k);
        results
    }

    pub fn delete(&mut self, id: &str) {
        self.embeddings.remove(id);
    }

    pub fn clear(&mut self) {
        self.embeddings.clear();
    }

    pub fn save_to_disk(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let bytes = bincode::serialize(self)?;
        std::fs::write(path, bytes)?;
        Ok(())
    }

    pub fn load_from_disk(path: &Path) -> Result<Self> {
        let bytes = std::fs::read(path)?;
        let store: Self = bincode::deserialize(&bytes)?;
        Ok(store)
    }
}
