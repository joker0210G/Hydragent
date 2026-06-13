use hydragent_planner::decomposer::{classify_complexity, TaskComplexity};
use hydragent_planner::dag::{DagSpec, DagNode, DagEdge, TaskType, NodeStatus};
use hydragent_planner::scheduler::ReadyQueue;
use hydragent_planner::serializer::{save_dag, load_dag};
use std::io::{self, Write};
use std::fs;

fn get_input(prompt: &str) -> String {
    print!("{}", prompt);
    let _ = io::stdout().flush();
    let mut input = String::new();
    io::stdin().read_line(&mut input).unwrap();
    input.trim().to_string()
}

fn make_node(id: &str, name: &str, desc: &str, task_type: TaskType) -> DagNode {
    DagNode {
        id: id.to_string(),
        name: name.to_string(),
        description: desc.to_string(),
        task_type,
        allowed_tools: vec!["web_search".to_string()],
        model_hint: None,
        token_budget: 2000,
        timeout_ms: 30000,
        retry_count: 0,
        max_retries: 2,
        status: NodeStatus::Pending,
        result: None,
    }
}

fn run_interactive_simulation(spec: DagSpec) {
    println!("\n=============================================");
    println!("Step 1: Saving planning graph to disk...");
    save_dag(&spec).unwrap();
    println!("✓ Saved to ./data/swarm/{}/dag.json", spec.swarm_id);

    println!("\nStep 2: Simulating server restart (Loading from disk)...");
    let mut spec = load_dag(&spec.swarm_id).unwrap();
    println!("✓ Successfully loaded planning graph back into memory!");

    println!("\n=============================================");
    println!("Step 3: Interactive Swarm Execution Loop");
    println!("You are now the Swarm Coordinator! You will see which sub-agents are ready to work.");
    println!("=============================================");

    loop {
        // Show status of all nodes
        println!("\n--- Swarm Status ---");
        for node in &spec.nodes {
            let status_symbol = match node.status {
                NodeStatus::Pending => "⏳ Pending",
                NodeStatus::Ready => "🟢 Ready",
                NodeStatus::Running => "🔄 Running",
                NodeStatus::Completed => "✅ Completed",
                NodeStatus::Failed => "❌ Failed",
                NodeStatus::Skipped => "⏭️ Skipped",
            };
            println!("  [{}] Node {} ({}): {}", status_symbol, node.id, node.name, node.description);
        }

        // Get nodes that are ready to run
        let ready_ids = {
            let queue = ReadyQueue::new(&mut spec);
            queue.get_ready_nodes()
        };

        if ready_ids.is_empty() {
            // Check if any are running/pending
            let has_incomplete = spec.nodes.iter().any(|n| n.status != NodeStatus::Completed && n.status != NodeStatus::Skipped);
            if has_incomplete {
                println!("\n⚠️ No nodes are ready to run, but some are still incomplete! (Likely blocked by a dependency error).");
            } else {
                println!("\n🎉 All tasks in the graph have completed successfully! Swarm execution done.");
            }
            break;
        }

        println!("\n🚀 Sub-agents ready for execution (All parent tasks finished):");
        for (i, id) in ready_ids.iter().enumerate() {
            if let Some(node) = spec.nodes.iter().find(|n| n.id == *id) {
                println!("  {}) Node ID: {} (Task: {})", i + 1, id, node.name);
            }
        }

        println!("\nOptions: ");
        println!("  - Type a number (e.g. 1) to complete that sub-agent's task.");
        println!("  - Or type a node ID (e.g. B) — same thing, by id instead of number.");
        println!("  - Type 'exit' to stop the simulation.");

        let choice_raw = get_input("\nYour choice › ");
        let choice = choice_raw.trim();

        if choice.eq_ignore_ascii_case("exit") {
            println!("Exiting simulation. Goodbye!");
            break;
        }

        if choice.is_empty() {
            println!("(no input — type a number, a node ID, or 'exit')");
            continue;
        }

        // Path 1: numeric 1..=N.
        if let Ok(idx) = choice.parse::<usize>() {
            if idx >= 1 && idx <= ready_ids.len() {
                let completed_id = &ready_ids[idx - 1];
                println!("\nExecuting task: {}...", completed_id);
                let mut queue = ReadyQueue::new(&mut spec);
                queue.update_status(completed_id, NodeStatus::Completed);
                println!("✓ Task {} marked COMPLETED!", completed_id);
                continue;
            }
            println!(
                "❌ '{}' is out of range — please choose 1-{} (or a node ID, or 'exit').",
                idx, ready_ids.len()
            );
            continue;
        }

        // Path 2: node id (case-insensitive). Must be in the ready queue;
        // if it isn't, explain *why* it isn't (already done? still waiting?).
        let upper = choice.to_ascii_uppercase();
        if let Some(pos) = ready_ids.iter().position(|id| id.eq_ignore_ascii_case(&upper)) {
            let completed_id = &ready_ids[pos];
            println!("\nExecuting task: {}...", completed_id);
            let mut queue = ReadyQueue::new(&mut spec);
            queue.update_status(completed_id, NodeStatus::Completed);
            println!("✓ Task {} marked COMPLETED!", completed_id);
            continue;
        }

        // The input wasn't numeric and wasn't a ready node id. If it
        // *looks* like a node id (single letter), tell the user why
        // that node isn't actionable. Otherwise give a generic hint.
        if choice.len() == 1 && choice.chars().next().unwrap().is_ascii_alphabetic() {
            let known = spec.nodes.iter().find(|n| n.id.eq_ignore_ascii_case(&upper));
            match known {
                Some(n) if n.status == NodeStatus::Completed => {
                    println!("❌ Node {} is already completed.", upper)
                }
                Some(n) if n.status == NodeStatus::Skipped => {
                    println!("❌ Node {} was skipped.", upper)
                }
                Some(n) if n.status == NodeStatus::Failed => {
                    println!("❌ Node {} is in a Failed state.", upper)
                }
                Some(_) => println!(
                    "❌ Node {} is not yet ready — it's waiting on its parent tasks.",
                    upper
                ),
                None => println!("❌ '{}' is not a known node id in this graph.", upper),
            }
            continue;
        }

        println!("❌ I didn't understand '{}'.", choice);
        println!(
            "   Type a number (1-{}), a node ID ({}), or 'exit'.",
            ready_ids.len(),
            ready_ids
                .iter()
                .map(|id| id.as_str())
                .collect::<Vec<_>>()
                .join("/")
        );
    }

    // Cleanup files
    let _ = fs::remove_file(format!("./data/swarm/{}/dag.json", spec.swarm_id));
    let _ = fs::remove_dir(format!("./data/swarm/{}", spec.swarm_id));
}

fn main() {
    println!("====================================================");
    println!("🐉 Hydragent Swarm Planner - Interactive Test Demo 🐉");
    println!("====================================================");
    println!("This tool lets you see how the AI plans and executes complex tasks step-by-step!");
    println!();

    let task = get_input("What project do you want to plan? › ");
    println!();

    println!("Analyzing your request...");
    let complexity = classify_complexity(&task);

    match complexity {
        TaskComplexity::Simple => {
            println!("🔍 Classification: [SIMPLE]");
            println!("Description: This request is short and simple. The system skips the planning graph");
            println!("             and answers it directly to save time and API costs.");
            println!("\nDone! No planning graph was created.");
        }
        TaskComplexity::Complex => {
            println!("🔍 Classification: [COMPLEX]");
            println!("Description: This request is complex or contains multiple steps! The system will");
            println!("             decompose this into a Directed Acyclic Graph (DAG).");
            println!();

            println!("Creating a sample project graph for: \"{}\"...", task);

            // Create a realistic sample project graph
            //    A (Research) -> B (Draft Article) -> D (Aggregate & Finish)
            //    A (Research) -> C (Gather Images) -> D (Aggregate & Finish)
            let swarm_id = "demo-swarm-session";
            let spec = DagSpec {
                swarm_id: swarm_id.to_string(),
                page_id: "demo-page-id".to_string(),
                original_task: task,
                nodes: vec![
                    make_node("A", "Research Information", "Find details and data for the topic", TaskType::Research),
                    make_node("B", "Draft Article", "Write the main article text based on research A", TaskType::CreativeWriting),
                    make_node("C", "Gather Images", "Find relevant pictures and diagrams based on research A", TaskType::Research),
                    make_node("D", "Assemble Document", "Combine draft B and images C into the final page", TaskType::Summarization),
                ],
                edges: vec![
                    DagEdge { from: "A".to_string(), to: "B".to_string(), label: Some("feeds research to draft".into()) },
                    DagEdge { from: "A".to_string(), to: "C".to_string(), label: Some("feeds topic to image gatherer".into()) },
                    DagEdge { from: "B".to_string(), to: "D".to_string(), label: Some("feeds draft to assembler".into()) },
                    DagEdge { from: "C".to_string(), to: "D".to_string(), label: Some("feeds images to assembler".into()) },
                ],
                created_at: chrono::Utc::now().timestamp_millis(),
            };

            run_interactive_simulation(spec);
        }
    }
}
