//! Swarm DAG execution — the "delegate_to_swarm" strategy body.
//!
//! Given a [`DagSpec`] (built by [`hydragent_planner::decomposer::decompose`]),
//! this module walks the DAG in topological order and runs each node as
//! a **tool-using sub-agent** (a lightweight ReAct loop). Each sub-agent
//! has its own role-specific system prompt, sees the upstream context,
//! and can call any registered tool (typically `web_search`,
//! `memory_search`, `file_read`) up to N times before producing a final
//! answer.
//!
//! ## Why tool-using sub-agents?
//!
//! v0.5.0 exposes the full tool catalog to the swarm so the LLM can
//! verify external facts (web search), look up local context (memory,
//! file system) instead of hallucinating them. The sub-agent runner
//! here mirrors [`crate::react_loop`]'s structured-output protocol:
//! the LLM emits one of:
//!
//!   * `{"action": "tool_name", "params": {...}}`  — call a tool
//!   * `{"final_answer": "..."}`                    — wrap up
//!
//! The runner executes tool calls (using [`ToolRegistry`]) and feeds
//! results back as a follow-up message, capping iterations to prevent
//! runaway loops.
//!
//! ## Streaming
//!
//! Every step's output is streamed through `response_tx` as
//! `response.token` events (the same protocol the ReAct loop uses), so
//! the CLI shows the swarm's progress in real time. The final summary
//! is returned to the caller for persistence into the page's history.

use anyhow::Result;
use hydragent_model::openrouter::ChatMessage;
use hydragent_model::router::ModelRouter;
use hydragent_planner::dag::{DagNode, DagSpec, NodeResult, NodeStatus};
use hydragent_tools::registry::ToolRegistry;
use hydragent_types::{ToolCall};
use serde_json::json;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{error, info, warn};
use uuid::Uuid;

use crate::strategy::extract_json;

/// Hard cap on tool-call iterations per sub-agent. Three is enough to
/// "look up the topic, look up the second source, then answer."
const MAX_TOOL_ITERATIONS: usize = 3;

/// Full swarm pipeline: decompose a task into a DAG, save it, then run it.
///
/// This is the public entry point used by the orchestrator. It glues
/// together:
///   1. `hydragent_planner::decomposer::decompose` (LLM → DagSpec)
///   2. `run_dag_sequential` (DagSpec → final summary, with tools)
///
/// Returns the swarm's final user-facing summary (or an error).
pub async fn run_swarm(
    page_id: &str,
    task: &str,
    model_router: Arc<ModelRouter>,
    tool_registry: Arc<ToolRegistry>,
    response_tx: mpsc::Sender<String>,
) -> Result<String> {
    let swarm_id = format!("swarm-{}", chrono::Utc::now().timestamp_millis());
    let spec = hydragent_planner::decomposer::decompose(
        &swarm_id,
        page_id,
        task,
        model_router.as_ref(),
    )
    .await?;
    run_dag_sequential(spec, model_router, tool_registry, response_tx).await
}


/// Execute a DAG end-to-end and return the final summary text.
///
/// The function streams progress through `response_tx` (the same channel
/// the ReAct loop uses) so the CLI sees:
///   1. `[Strategy: delegate_to_swarm]` (sent by the orchestrator)
///   2. `[Swarm: N nodes, M edges]`     (sent here)
///   3. `[Sub-agent: <name> (<task_type>)] ...` and the streamed output
///   4. `Swarm completed. N/M nodes succeeded.`
pub async fn run_dag_sequential(
    spec: DagSpec,
    model_router: Arc<ModelRouter>,
    tool_registry: Arc<ToolRegistry>,
    response_tx: mpsc::Sender<String>,
) -> Result<String> {
    let total = spec.nodes.len();
    let edges = spec.edges.len();
    let swarm_id = spec.swarm_id.clone();

    info!(swarm_id = %swarm_id, nodes = total, edges, "Swarm: starting DAG execution");

    // 1. Save the initial spec to disk (audit trail).
    save_spec(&spec)?;

    // 2. Announce the swarm structure to the user.
    let _ = response_tx
        .send(
            json!({
                "jsonrpc": "2.0",
                "method": "response.status",
                "params": {
                    "status": format!(
                        "\n`[Swarm: {} nodes, {} edges — saved to ./data/swarm/{}]`\n",
                        total, edges, swarm_id
                    )
                }
            })
            .to_string(),
        )
        .await;

    // 3. Topologically order the nodes.
    let order = topo_order(&spec)?;
    info!("Swarm: topological order = {:?}", order);

    // 4. Execute each node in order, collecting outputs.
    let mut outputs: HashMap<String, String> = HashMap::new();
    let mut succeeded: usize = 0;
    let mut failed: usize = 0;
    let mut skipped: usize = 0;
    let mut updated_nodes = spec.nodes.clone();
    let mut node_by_id: HashMap<String, DagNode> =
        updated_nodes.iter().map(|n| (n.id.clone(), n.clone())).collect();

    for node_id in &order {
        // Take an owned copy of the node so we can mutate it freely
        // and re-insert at the end without fighting the borrow checker.
        let mut node = match node_by_id.get(node_id) {
            Some(n) => n.clone(),
            None => {
                warn!("Swarm: node {} not found in spec — skipping", node_id);
                skipped += 1;
                continue;
            }
        };

        // Pull in upstream outputs.
        let upstream = collect_upstream_outputs(node_id, &spec, &outputs);
        let prompt = build_node_prompt(&node, &upstream, &spec.original_task);

        // Mark as running.
        node.status = NodeStatus::Running;

        // Announce this sub-agent.
        let _ = response_tx
            .send(
                json!({
                    "jsonrpc": "2.0",
                    "method": "response.status",
                    "params": {
                        "status": format!(
                            "\n[Sub-agent: {} ({:?})]\n",
                            node.name, node.task_type
                        )
                    }
                })
                .to_string(),
            )
            .await;

        // Run the sub-agent (mini ReAct loop with tools). The system
        // prompt advertises the available tools, and the LLM can call
        // them up to MAX_TOOL_ITERATIONS times before producing a final
        // answer.
        let (content, used_model) = match run_sub_agent(
            &model_router,
            &tool_registry,
            &node,
            &prompt,
            response_tx.clone(),
        )
        .await
        {
            Ok(t) => t,
            Err(e) => {
                error!("Swarm: node {} sub-agent failed: {}", node_id, e);
                node.status = NodeStatus::Failed;
                node.result = Some(NodeResult {
                    content: format!("[error: {}]", e),
                    model_used: "error".to_string(),
                    tokens_used: 0,
                    execution_ms: 0,
                });
                node_by_id.insert(node_id.clone(), node);
                failed += 1;
                continue;
            }
        };

        // Mark as completed and store the result.
        node.status = NodeStatus::Completed;
        node.result = Some(NodeResult {
            content: content.clone(),
            model_used: used_model.clone(),
            tokens_used: 0, // best-effort; router doesn't expose this in this runner
            execution_ms: 0,
        });
        outputs.insert(node_id.clone(), content);
        succeeded += 1;

        // Persist the updated node back into the index.
        node_by_id.insert(node_id.clone(), node);
    }

    // 5. Reassemble the spec from the updated nodes and save it.
    updated_nodes = node_by_id.values().cloned().collect();
    let final_spec = DagSpec {
        swarm_id: spec.swarm_id.clone(),
        page_id: spec.page_id.clone(),
        original_task: spec.original_task.clone(),
        nodes: updated_nodes,
        edges: spec.edges.clone(),
        created_at: spec.created_at,
    };
    save_spec(&final_spec)?;

    // 6. Synthesize a final summary (last node's output, or a
    //    constructed message if no nodes succeeded).
    let final_summary = build_final_summary(&final_spec, &outputs, succeeded, failed, skipped);

    info!(
        swarm_id = %swarm_id,
        succeeded, failed, skipped,
        "Swarm: DAG execution complete"
    );

    let _ = response_tx
        .send(
            json!({
                "jsonrpc": "2.0",
                "method": "response.status",
                "params": {
                    "status": format!(
                        "\n`[Swarm complete: {} ok, {} failed, {} skipped of {}]`\n",
                        succeeded, failed, skipped, total
                    )
                }
            })
            .to_string(),
        )
        .await;

    Ok(final_summary)
}

// ── helpers ────────────────────────────────────────────────────────────

/// Topologically order the node ids of a DagSpec. We use the same
/// petgraph-backed validator that the planner already uses.
fn topo_order(spec: &DagSpec) -> Result<Vec<String>> {
    let graph = spec.validate()?;
    use petgraph::algo::toposort;
    let sorted = toposort(&graph, None)
        .map_err(|e| anyhow::anyhow!("DAG has a cycle: {:?}", e))?;
    Ok(sorted
        .into_iter()
        .map(|idx| graph[idx].clone())
        .collect())
}

fn collect_upstream_outputs(
    node_id: &str,
    spec: &DagSpec,
    outputs: &HashMap<String, String>,
) -> HashMap<String, String> {
    let mut upstream = HashMap::new();
    for edge in &spec.edges {
        if edge.to == node_id {
            if let Some(out) = outputs.get(&edge.from) {
                upstream.insert(edge.from.clone(), out.clone());
            }
        }
    }
    upstream
}

fn build_node_prompt(
    node: &DagNode,
    upstream: &HashMap<String, String>,
    original_task: &str,
) -> String {
    let mut prompt = String::new();
    prompt.push_str(&format!(
        "You are a specialist sub-agent in an AI swarm.\n\
         Original user request: \"{}\"\n\n\
         Your specific task: {}\n",
        original_task, node.description
    ));

    if !upstream.is_empty() {
        prompt.push_str("\n## Inputs from upstream tasks\n\n");
        let mut keys: Vec<&String> = upstream.keys().collect();
        keys.sort();
        for k in keys {
            prompt.push_str(&format!("### {}\n{}\n\n", k, upstream[k]));
        }
    }

    prompt.push_str(
        "\n## Instructions\n\
         - Complete your specific task above.\n\
         - Use the upstream outputs as context if relevant.\n\
         - You may call the tools listed in the system prompt to verify facts.\n\
         - Produce a complete, self-contained answer — downstream sub-agents will not see this conversation.\n",
    );

    prompt
}

// ── sub-agent runner (mini ReAct loop with tools) ─────────────────────

/// Run a sub-agent as a mini ReAct loop with tool access.
///
/// Protocol:
///   1. Send the system prompt (role + available tools) and the
///      user prompt (task + upstream context).
///   2. Read the LLM's response. The LLM should reply with JSON of one
///      of two shapes:
///        * `{"action": "<tool>", "params": {...}}`  → execute, feed result back
///        * `{"final_answer": "..."}`                → wrap up
///   3. If the LLM emits neither (rare, or model "thinking aloud"),
///      treat the response itself as the final answer.
///   4. Cap at [`MAX_TOOL_ITERATIONS`] tool calls to bound runtime.
async fn run_sub_agent(
    model_router: &ModelRouter,
    tool_registry: &ToolRegistry,
    node: &DagNode,
    user_prompt: &str,
    response_tx: mpsc::Sender<String>,
) -> Result<(String, String)> {
    let system_prompt = build_sub_agent_system_prompt(node, tool_registry);

    let mut messages: Vec<ChatMessage> = vec![
        ChatMessage { role: "system".to_string(), content: system_prompt },
        ChatMessage { role: "user".to_string(),  content: user_prompt.to_string() },
    ];

    let mut iterations = 0usize;
    let mut final_answer: Option<String> = None;
    let mut last_model = String::new();

    while iterations < MAX_TOOL_ITERATIONS {
        iterations += 1;

        // Call the LLM with the current message history. chat_stream
        // returns the full accumulated content even though it also
        // streams tokens to `response_tx` for the user to see.
        let (content, model_used) = model_router
            .chat_stream(messages.clone(), response_tx.clone(), None)
            .await?;
        last_model = model_used;

        // Try to parse a tool call.
        if let Some((tool_name, params_json, call_id)) =
            parse_tool_invocation(&content)
        {
            info!(
                "Sub-agent {} iter {}: tool_call tool={}",
                node.id, iterations, tool_name
            );

            // Build a ToolCall and invoke.
            let tool_call = ToolCall {
                call_id: call_id.clone(),
                tool_id: tool_name.clone(),
                params_json: params_json.clone(),
                tier: tool_registry
                    .get_tier(&tool_name),
            };

            // Announce the tool call to the user as a status line.
            let announce = json!({
                "jsonrpc": "2.0",
                "method": "response.status",
                "params": {
                    "status": format!(
                        "\n  ↳ [Tool call: {} {}] → executing…\n",
                        tool_name, params_json
                    )
                }
            });
            let _ = response_tx.send(announce.to_string()).await;

            let result = tool_registry.invoke(&tool_call).await;

            // Surface the tool result back to the LLM as a "user"
            // message. We use a structured prefix so the LLM can
            // distinguish tool output from real user input.
            let result_text = format!(
                "## Tool result for {} (call_id={})\nstatus: {}\n```json\n{}\n```\n{}",
                tool_name,
                call_id,
                match result.status {
                    hydragent_types::ToolStatus::Success => "success",
                    hydragent_types::ToolStatus::Failure => "failure",
                    hydragent_types::ToolStatus::Timeout => "timeout",
                },
                result.output_json,
                result
                    .error_message
                    .as_deref()
                    .map(|m| format!("\nError: {}", m))
                    .unwrap_or_default(),
            );
            messages.push(ChatMessage { role: "assistant".to_string(), content });
            messages.push(ChatMessage { role: "user".to_string(),    content: result_text });
            continue;
        }

        // No tool call: try to parse a final answer.
        if let Some(answer) = parse_final_answer(&content) {
            final_answer = Some(answer);
            break;
        }

        // Nothing structured: treat the raw content as the final
        // answer (graceful degradation for models that ignore the
        // JSON protocol).
        warn!(
            "Sub-agent {} iter {}: response not in JSON protocol — \
             treating as final answer ({} chars).",
            node.id,
            iterations,
            content.len()
        );
        final_answer = Some(content);
        break;
    }

    if final_answer.is_none() {
        warn!(
            "Sub-agent {} exhausted {} iterations without a final answer — using last content.",
            node.id,
            MAX_TOOL_ITERATIONS
        );
        // Pull the last assistant message as the answer.
        if let Some(last) = messages.iter().rev().find(|m| m.role == "assistant") {
            final_answer = Some(last.content.clone());
        }
    }

    Ok((final_answer.unwrap_or_default(), last_model))
}

/// Try to extract a `{"action": "...", "params": {...}}` tool call from
/// the LLM's raw text. Returns `(tool_name, params_json, call_id)` on
/// success.
fn parse_tool_invocation(text: &str) -> Option<(String, String, String)> {
    let json_str = extract_json(text)?;
    let v: serde_json::Value = serde_json::from_str(&json_str).ok()?;
    let action = v.get("action")?.as_str()?.to_string();
    let params = v.get("params").cloned().unwrap_or(json!({}));
    let call_id = Uuid::new_v4().to_string();
    Some((action, serde_json::to_string(&params).unwrap_or("{}".to_string()), call_id))
}

/// Try to extract a `{"final_answer": "..."}` payload. Returns the
/// answer string on success.
fn parse_final_answer(text: &str) -> Option<String> {
    let json_str = extract_json(text)?;
    let v: serde_json::Value = serde_json::from_str(&json_str).ok()?;
    v.get("final_answer")?.as_str().map(String::from)
}

/// Build the sub-agent's system prompt: role + tool catalog + JSON
/// output protocol.
fn build_sub_agent_system_prompt(node: &DagNode, registry: &ToolRegistry) -> String {
    let role = system_prompt_for_role(node);
    let tool_block = registry.build_system_prompt_block();

    format!(
        "{role}\n\n\
         ## Available Tools\n\
         {tools}\n\n\
         ## Output Protocol (strict)\n\
         You MUST respond with EXACTLY ONE of these JSON shapes (no markdown fences, no extra prose):\n\n\
         1. To call a tool:\n\
         {{\"action\": \"<tool_name>\", \"params\": {{ ... }}}}\n\n\
         2. To give your final answer:\n\
         {{\"final_answer\": \"<your complete, self-contained answer>\"}}\n\n\
         Rules:\n\
         - Maximum 3 tool calls before you MUST emit a `final_answer`.\n\
         - After receiving a tool result, decide whether to call another tool or wrap up.\n\
         - Be concise. The downstream sub-agents do not see your full conversation — your `final_answer` is all they get.\n",
        role = role,
        tools = if tool_block.is_empty() {
            "_(no tools registered — emit a `final_answer` directly)_".to_string()
        } else {
            tool_block
        },
    )
}

fn build_final_summary(
    spec: &DagSpec,
    outputs: &HashMap<String, String>,
    succeeded: usize,
    failed: usize,
    skipped: usize,
) -> String {
    if succeeded == 0 {
        return format!(
            "The swarm could not complete any sub-tasks ({} failed, {} skipped). \
             Please try rephrasing your request or breaking it into smaller steps.",
            failed, skipped
        );
    }

    // Prefer the longest completed output as the user-facing summary —
    // that's usually the "report" or "final write" node downstream
    // of the research nodes.
    let mut best: Option<(&String, &String)> = None;
    for (id, content) in outputs {
        if let Some(node) = spec.nodes.iter().find(|n| &n.id == id) {
            if node.status == NodeStatus::Completed {
                if best.map(|(_, c)| content.len() > c.len()).unwrap_or(true) {
                    best = Some((id, content));
                }
            }
        }
    }
    if let Some((id, content)) = best {
        format!(
            "{}\n\n---\n_Swarm: {} sub-agents completed. Final answer from node `{}`._",
            content.trim(),
            succeeded,
            id
        )
    } else {
        format!("Swarm completed {} sub-agents (no final synthesis).", succeeded)
    }
}

fn save_spec(spec: &DagSpec) -> Result<()> {
    let dir = format!("./data/swarm/{}", spec.swarm_id);
    std::fs::create_dir_all(&dir).ok();
    let path = format!("{}/dag.json", dir);
    let json = serde_json::to_string_pretty(spec)?;
    std::fs::write(&path, json)?;
    Ok(())
}

/// Free function (instead of an inherent method on the foreign
/// `DagNode` type, which would violate orphan rules) that builds a
/// role-specific system prompt for a sub-agent.
fn system_prompt_for_role(node: &DagNode) -> String {
    match node.task_type {
        hydragent_planner::dag::TaskType::Research => {
            "You are a research sub-agent. Find accurate, current information. \
             Always verify external facts (e.g. via web_search) before claiming them."
                .to_string()
        }
        hydragent_planner::dag::TaskType::CreativeWriting => {
            "You are a creative-writing sub-agent. Produce clear, engaging prose. \
             Match the user's tone and length expectations."
                .to_string()
        }
        hydragent_planner::dag::TaskType::Summarization => {
            "You are a summarization sub-agent. Distill the input into a concise, \
             accurate summary that preserves the key points."
                .to_string()
        }
        hydragent_planner::dag::TaskType::Reasoning => {
            "You are a reasoning sub-agent. Think step-by-step. Show your work."
                .to_string()
        }
        hydragent_planner::dag::TaskType::DataExtraction => {
            "You are a data-extraction sub-agent. Pull out structured facts from \
             unstructured input. Be precise."
                .to_string()
        }
        hydragent_planner::dag::TaskType::Planning => {
            "You are a planning sub-agent. Produce a clear, ordered plan with \
             dependencies."
                .to_string()
        }
        hydragent_planner::dag::TaskType::Review => {
            "You are a review sub-agent. Critique the input for correctness, clarity, \
             and completeness."
                .to_string()
        }
        hydragent_planner::dag::TaskType::CodeGeneration => {
            "You are a code-generation sub-agent. Write clean, well-commented code."
                .to_string()
        }
        hydragent_planner::dag::TaskType::General => {
            "You are a general-purpose sub-agent. Complete the task to the best of your ability."
                .to_string()
        }
    }
}

// Suppress unused warning for HashSet on platforms that don't use it.
#[allow(dead_code)]
fn _unused(_: HashSet<String>) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_tool_invocation_basic() {
        let text = r#"I'll search for it first. {"action": "web_search", "params": {"query": "Fabla 5"}}"#;
        let (name, params, _id) = parse_tool_invocation(text).expect("should parse");
        assert_eq!(name, "web_search");
        assert!(params.contains("Fabla 5"));
    }

    #[test]
    fn parse_tool_invocation_no_action() {
        let text = r#"{"final_answer": "hello"}"#;
        assert!(parse_tool_invocation(text).is_none());
    }

    #[test]
    fn parse_final_answer_basic() {
        let text = r#"All done. {"final_answer": "Paris is the capital of France."}"#;
        assert_eq!(
            parse_final_answer(text).as_deref(),
            Some("Paris is the capital of France.")
        );
    }

    #[test]
    fn parse_final_answer_missing() {
        let text = r#"{"action": "web_search", "params": {}}"#;
        assert!(parse_final_answer(text).is_none());
    }
}
