use hydragent_planner::dag::{DagSpec, DagNode, DagEdge, TaskType, NodeStatus};
use hydragent_planner::scheduler::ReadyQueue;
use hydragent_planner::serializer::{save_dag, load_dag};
use hydragent_planner::decomposer::{classify_complexity, TaskComplexity};
use std::fs;

fn make_dummy_node(id: &str) -> DagNode {
    DagNode {
        id: id.to_string(),
        name: format!("Node {}", id),
        description: "Test description".to_string(),
        task_type: TaskType::General,
        allowed_tools: vec![],
        model_hint: None,
        token_budget: 1000,
        timeout_ms: 10000,
        retry_count: 0,
        max_retries: 2,
        status: NodeStatus::Pending,
        result: None,
    }
}

#[test]
fn test_linear_chain_toposort() {
    // A -> B -> C -> D -> E
    let spec = DagSpec {
        swarm_id: "test-linear".to_string(),
        page_id: "test-page".to_string(),
        original_task: "test linear".to_string(),
        nodes: vec![
            make_dummy_node("E"),
            make_dummy_node("D"),
            make_dummy_node("C"),
            make_dummy_node("B"),
            make_dummy_node("A"),
        ],
        edges: vec![
            DagEdge { from: "A".to_string(), to: "B".to_string(), label: None },
            DagEdge { from: "B".to_string(), to: "C".to_string(), label: None },
            DagEdge { from: "C".to_string(), to: "D".to_string(), label: None },
            DagEdge { from: "D".to_string(), to: "E".to_string(), label: None },
        ],
        created_at: 0,
    };

    let order = spec.topological_order().unwrap();
    assert_eq!(order, vec!["A", "B", "C", "D", "E"]);
}

#[test]
fn test_diamond_dependencies() {
    //     A
    //    / \
    //   B   C
    //    \ /
    //     D
    let mut spec = DagSpec {
        swarm_id: "test-diamond".to_string(),
        page_id: "test-page".to_string(),
        original_task: "test diamond".to_string(),
        nodes: vec![
            make_dummy_node("A"),
            make_dummy_node("B"),
            make_dummy_node("C"),
            make_dummy_node("D"),
        ],
        edges: vec![
            DagEdge { from: "A".to_string(), to: "B".to_string(), label: None },
            DagEdge { from: "A".to_string(), to: "C".to_string(), label: None },
            DagEdge { from: "B".to_string(), to: "D".to_string(), label: None },
            DagEdge { from: "C".to_string(), to: "D".to_string(), label: None },
        ],
        created_at: 0,
    };

    // Assert that only A is ready initially
    {
        let queue = ReadyQueue::new(&mut spec);
        let ready = queue.get_ready_nodes();
        assert_eq!(ready, vec!["A"]);
    }

    // Mark A complete
    {
        let mut queue = ReadyQueue::new(&mut spec);
        queue.update_status("A", NodeStatus::Completed);
        let ready = queue.get_ready_nodes();
        // B and C are concurrent and ready to run
        assert!(ready.contains(&"B".to_string()));
        assert!(ready.contains(&"C".to_string()));
        assert_eq!(ready.len(), 2);
    }

    // Mark B complete, D still waits for C
    {
        let mut queue = ReadyQueue::new(&mut spec);
        queue.update_status("B", NodeStatus::Completed);
        let ready = queue.get_ready_nodes();
        assert_eq!(ready, vec!["C"]);
    }

    // Mark C complete, D is now ready
    {
        let mut queue = ReadyQueue::new(&mut spec);
        queue.update_status("C", NodeStatus::Completed);
        let ready = queue.get_ready_nodes();
        assert_eq!(ready, vec!["D"]);
    }
}

#[test]
fn test_cycle_detection() {
    // A -> B -> A
    let spec = DagSpec {
        swarm_id: "test-cycle".to_string(),
        page_id: "test-page".to_string(),
        original_task: "test cycle".to_string(),
        nodes: vec![
            make_dummy_node("A"),
            make_dummy_node("B"),
        ],
        edges: vec![
            DagEdge { from: "A".to_string(), to: "B".to_string(), label: None },
            DagEdge { from: "B".to_string(), to: "A".to_string(), label: None },
        ],
        created_at: 0,
    };

    let result = spec.validate();
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("cycle"));
}

#[test]
fn test_json_serialization_roundtrip() {
    let swarm_id = "test-serialize-123";
    let spec = DagSpec {
        swarm_id: swarm_id.to_string(),
        page_id: "page-xyz".to_string(),
        original_task: "serial test".to_string(),
        nodes: vec![
            make_dummy_node("A"),
        ],
        edges: vec![],
        created_at: 1234567890,
    };

    save_dag(&spec).unwrap();
    let loaded = load_dag(swarm_id).unwrap();

    assert_eq!(loaded.swarm_id, spec.swarm_id);
    assert_eq!(loaded.page_id, spec.page_id);
    assert_eq!(loaded.original_task, spec.original_task);
    assert_eq!(loaded.nodes.len(), 1);
    assert_eq!(loaded.nodes[0].id, "A");
    assert_eq!(loaded.created_at, spec.created_at);

    // Clean up
    let _ = fs::remove_file(format!("./data/swarm/{}/dag.json", swarm_id));
    let _ = fs::remove_dir(format!("./data/swarm/{}", swarm_id));
}

#[test]
fn test_complexity_classifier() {
    // Simple cases
    assert_eq!(classify_complexity("hello"), TaskComplexity::Simple);
    assert_eq!(classify_complexity("find Tokyo time"), TaskComplexity::Simple);
    
    // Complex cases due to token count (> 40)
    let long_task = "this is a very long task description that has many words to make sure we exceed the forty word threshold that we have configured in our heuristic complexity check method to identify large tasks and ensure that the planning engine triggers correctly for it";
    assert_eq!(classify_complexity(long_task), TaskComplexity::Complex);

    // Complex cases due to connectives
    assert_eq!(classify_complexity("do research and then write report"), TaskComplexity::Complex);
    assert_eq!(classify_complexity("compare both frameworks"), TaskComplexity::Complex);
}
