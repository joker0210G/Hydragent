use crate::dag::DagSpec;
use std::fs;
use std::path::Path;

/// Save a DagSpec as JSON in the data directory under data/swarm/{swarm_id}/dag.json
pub fn save_dag(spec: &DagSpec) -> anyhow::Result<()> {
    let path_str = format!("./data/swarm/{}/dag.json", spec.swarm_id);
    let path = Path::new(&path_str);
    
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    
    let serialized = serde_json::to_string_pretty(spec)?;
    fs::write(path, serialized)?;
    
    Ok(())
}

/// Load a DagSpec from the JSON file at data/swarm/{swarm_id}/dag.json
pub fn load_dag(swarm_id: &str) -> anyhow::Result<DagSpec> {
    let path_str = format!("./data/swarm/{}/dag.json", swarm_id);
    let path = Path::new(&path_str);
    
    let content = fs::read_to_string(path)?;
    let spec: DagSpec = serde_json::from_str(&content)?;
    
    Ok(spec)
}
