use crate::dag::{DagSpec, NodeStatus};
use std::collections::HashSet;

pub struct ReadyQueue<'a> {
    spec: &'a mut DagSpec,
}

impl<'a> ReadyQueue<'a> {
    pub fn new(spec: &'a mut DagSpec) -> Self {
        Self { spec }
    }

    /// Computes which nodes are currently ready to run.
    /// A node is ready if its status is `Pending` and all its parent dependencies are `Completed` or `Skipped`.
    pub fn get_ready_nodes(&self) -> Vec<String> {
        let completed_nodes: HashSet<String> = self.spec.nodes.iter()
            .filter(|n| n.status == NodeStatus::Completed || n.status == NodeStatus::Skipped)
            .map(|n| n.id.clone())
            .collect();

        let mut ready_nodes = Vec::new();

        for node in &self.spec.nodes {
            if node.status != NodeStatus::Pending {
                continue;
            }

            // A node is ready if all incoming edges are from completed nodes
            let mut all_deps_met = true;
            for edge in &self.spec.edges {
                if edge.to == node.id {
                    if !completed_nodes.contains(&edge.from) {
                        all_deps_met = false;
                        break;
                    }
                }
            }

            if all_deps_met {
                ready_nodes.push(node.id.clone());
            }
        }

        ready_nodes
    }

    /// Update status of a node.
    pub fn update_status(&mut self, node_id: &str, status: NodeStatus) {
        if let Some(node) = self.spec.nodes.iter_mut().find(|n| n.id == node_id) {
            node.status = status;
        }
    }
}
