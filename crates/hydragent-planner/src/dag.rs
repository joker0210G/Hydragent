use serde::{Deserialize, Serialize};
use petgraph::graph::{DiGraph, NodeIndex};
use petgraph::algo::{is_cyclic_directed, toposort};
use std::collections::HashMap;

/// A single unit of work in the swarm's execution plan.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DagNode {
    pub id: String,                       // UUID or node id
    pub name: String,                     // Human-readable (e.g., "Research Rust frameworks")
    pub description: String,              // Detailed task for the sub-agent
    pub task_type: TaskType,              // For Model Council routing
    pub allowed_tools: Vec<String>,       // Tool whitelist for this node's sub-agent
    pub model_hint: Option<String>,       // Override Model Council (e.g., "openai/gpt-4o")
    pub token_budget: u32,                // Max tokens this node may consume
    pub timeout_ms: u64,                  // Wall-clock timeout
    pub retry_count: u32,                 // Current retry attempt (starts at 0)
    pub max_retries: u32,                 // Maximum allowed retries before escalation
    pub status: NodeStatus,               // Lifecycle state
    pub result: Option<NodeResult>,       // Populated on completion
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NodeStatus {
    Pending,     // Waiting on dependencies
    Ready,       // All dependencies satisfied; awaiting dispatch
    Running,     // Sub-agent is active
    Completed,   // Successful result
    Failed,      // Terminal failure
    Skipped,     // Bypassed by re-planner
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskType {
    CodeGeneration,
    Research,
    CreativeWriting,
    Reasoning,
    Summarization,
    DataExtraction,
    Planning,
    Review,
    General,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeResult {
    pub content: String,              // The sub-agent's output
    pub model_used: String,           // Which model produced this result
    pub tokens_used: u32,
    pub execution_ms: u64,
}

/// A directed edge representing a dependency: `from` must complete before `to` can start.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DagEdge {
    pub from: String,                 // Source node ID
    pub to: String,                   // Target node ID
    pub label: Option<String>,        // edge label/annotation
}

/// The full task graph, plus swarm-level metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DagSpec {
    pub swarm_id: String,
    pub page_id: String,              // Aligned with page-centric design
    pub original_task: String,
    pub nodes: Vec<DagNode>,
    pub edges: Vec<DagEdge>,
    pub created_at: i64,
}

impl DagSpec {
    /// Build a petgraph DiGraph from this spec and validate it.
    pub fn validate(&self) -> anyhow::Result<DiGraph<String, ()>> {
        let mut graph = DiGraph::new();
        let mut node_indices: HashMap<String, NodeIndex> = HashMap::new();

        for node in &self.nodes {
            let idx = graph.add_node(node.id.clone());
            node_indices.insert(node.id.clone(), idx);
        }

        for edge in &self.edges {
            let from = node_indices.get(&edge.from)
                .ok_or_else(|| anyhow::anyhow!("Edge references unknown node: {}", edge.from))?;
            let to = node_indices.get(&edge.to)
                .ok_or_else(|| anyhow::anyhow!("Edge references unknown node: {}", edge.to))?;
            graph.add_edge(*from, *to, ());
        }

        if is_cyclic_directed(&graph) {
            anyhow::bail!("DAG contains a cycle — invalid task graph");
        }

        Ok(graph)
    }

    /// Return nodes in a valid topological execution order.
    pub fn topological_order(&self) -> anyhow::Result<Vec<String>> {
        let graph = self.validate()?;
        let sorted = toposort(&graph, None)
            .map_err(|_| anyhow::anyhow!("Cycle detected during topological sort"))?;
        Ok(sorted.into_iter().map(|idx| graph[idx].clone()).collect())
    }
}
