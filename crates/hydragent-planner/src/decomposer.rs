use crate::dag::{DagSpec, DagNode, DagEdge};
use hydragent_model::router::ModelRouter;

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum TaskComplexity {
    Simple,
    Complex,
}

/// Simple vs Complex task classifier heuristic.
pub fn classify_complexity(task: &str) -> TaskComplexity {
    let token_count = task.split_whitespace().count();
    let has_compound_connectives = [
        "and then", "after that", "also", "additionally", "furthermore",
        "first", "second", "finally", "compare", "both", "all three"
    ]
    .iter()
    .any(|kw| task.to_lowercase().contains(kw));

    if token_count > 40 || has_compound_connectives {
        TaskComplexity::Complex
    } else {
        TaskComplexity::Simple
    }
}

pub const DECOMPOSITION_SYSTEM_PROMPT: &str = r#"You are a task decomposition expert for an AI agent swarm. Your job is to break a complex user request into a minimal set of sub-tasks that can be executed by specialist AI agents.

RULES:
1. Each sub-task should be executable by a single specialist agent with one focus.
2. Identify dependencies: if Task B requires output from Task A, add an edge A -> B.
3. Maximize parallelism: tasks without dependencies should have NO edges between them.
4. Assign a task_type to each node from: code_generation | research | creative_writing | reasoning | summarization | data_extraction | planning | review | general
5. Assign appropriate tools to each node. Available tools: web_search, file_read, file_write, code_exec, memory_search, memory_store, wiki_write, delegate_task
6. Keep the DAG as SMALL as possible (3–8 nodes for most tasks).
7. Output ONLY valid JSON containing "nodes" and "edges". No explanation, markdown codeblocks, or extra text.

OUTPUT SCHEMA:
{
  "nodes": [
    {
      "id": "node-1",
      "name": "Short name",
      "description": "Detailed task description for the sub-agent",
      "task_type": "research",
      "allowed_tools": ["web_search", "memory_search"],
      "token_budget": 4000,
      "timeout_ms": 30000,
      "max_retries": 2
    }
  ],
  "edges": [
    { "from": "node-1", "to": "node-3", "label": "research feeds into writing" }
  ]
}

FEW-SHOT EXAMPLE:
Task: "Find the top 3 Rust web frameworks, compare them, and write a recommendation."
Output:
{
  "nodes": [
    {"id":"n1","name":"Research Actix-Web","description":"Search for Actix-Web features, performance benchmarks, and community adoption","task_type":"research","allowed_tools":["web_search","memory_search"],"token_budget":3000,"timeout_ms":20000,"max_retries":2},
    {"id":"n2","name":"Research Axum","description":"Search for Axum features, Tokio integration, and ecosystem","task_type":"research","allowed_tools":["web_search","memory_search"],"token_budget":3000,"timeout_ms":20000,"max_retries":2},
    {"id":"n3","name":"Research Warp","description":"Search for Warp features, filter system, and use cases","task_type":"research","allowed_tools":["web_search","memory_search"],"token_budget":3000,"timeout_ms":20000,"max_retries":2},
    {"id":"n4","name":"Comparison Table","description":"Using research from n1, n2, n3, create a markdown comparison table","task_type":"data_extraction","allowed_tools":["memory_search"],"token_budget":2000,"timeout_ms":15000,"max_retries":1},
    {"id":"n5","name":"Write Recommendation","description":"Write a 200-word recommendation based on the comparison","task_type":"creative_writing","allowed_tools":[],"token_budget":1500,"timeout_ms":15000,"max_retries":1}
  ],
  "edges": [
    {"from":"n1","to":"n4"},{"from":"n2","to":"n4"},{"from":"n3","to":"n4"},{"from":"n4","to":"n5"}
  ]
}"#;

/// Call the LLM to decompose a task into a DagSpec.
pub async fn decompose(
    swarm_id: &str,
    page_id: &str,
    original_task: &str,
    llm: &ModelRouter,
) -> anyhow::Result<DagSpec> {
    let prompt = format!(
        "{}\n\nUSER TASK TO DECOMPOSE:\n{}\n\nOUTPUT JSON:",
        DECOMPOSITION_SYSTEM_PROMPT,
        original_task
    );

    let raw_json = llm.generate_non_streaming(&prompt, None).await
        .map_err(|e| anyhow::anyhow!("Decomposition LLM call failed: {}", e))?;

    let json_str = extract_json(&raw_json)?;

    #[derive(serde::Deserialize)]
    struct RawSpec {
        nodes: Vec<DagNode>,
        edges: Vec<DagEdge>,
    }
    
    // Deserialize raw lists
    let raw: RawSpec = serde_json::from_str(&json_str)
        .map_err(|e| anyhow::anyhow!("Failed to parse decomposition JSON: {}. Raw: {}", e, &json_str[..json_str.len().min(200)]))?;

    // Map raw nodes into our DagNode (adding default lifecycle attributes)
    let nodes: Vec<crate::dag::DagNode> = raw.nodes.into_iter().map(|mut n| {
        n.status = crate::dag::NodeStatus::Pending;
        n.retry_count = 0;
        n.result = None;
        n
    }).collect();

    let spec = DagSpec {
        swarm_id: swarm_id.to_string(),
        page_id: page_id.to_string(),
        original_task: original_task.to_string(),
        nodes,
        edges: raw.edges,
        created_at: chrono::Utc::now().timestamp_millis(),
    };

    // Validate before returning
    spec.validate().map_err(|e| anyhow::anyhow!("Decomposition produced invalid DAG: {}", e))?;

    tracing::info!(
        swarm_id,
        node_count = spec.nodes.len(),
        edge_count = spec.edges.len(),
        "Task decomposed into DAG"
    );

    Ok(spec)
}

fn extract_json(s: &str) -> anyhow::Result<String> {
    if let (Some(start), Some(end)) = (s.find('{'), s.rfind('}')) {
        Ok(s[start..=end].to_string())
    } else {
        anyhow::bail!("No JSON object found in LLM response")
    }
}
