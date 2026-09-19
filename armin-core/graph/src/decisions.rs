use std::collections::{HashSet, VecDeque};

use petgraph::visit::EdgeRef;
use petgraph::Direction;

use crate::store::GraphStoreInner;
use crate::types::{Decision, EdgeType, NodeStatus, NodeType};
use crate::utils::session_short_name;

/// Extract decisions with status, rationale, blocked work, and validation criteria.
pub fn extract_decisions(inner: &GraphStoreInner, _current_session_idx: usize) -> Vec<Decision> {
    let mut results = Vec::new();

    for node_idx in inner.graph.node_indices() {
        let node = &inner.graph[node_idx];
        if node.node_type != NodeType::Decision {
            continue;
        }
        if node.status == NodeStatus::Invalidated {
            continue;
        }

        let mut has_support = false;
        let mut has_contradiction = false;
        let mut is_superseded = false;
        let mut rationale_parts: Vec<String> = Vec::new();

        for edge_ref in inner.graph.edges_directed(node_idx, Direction::Incoming) {
            let edge = edge_ref.weight();
            match edge.edge_type {
                EdgeType::RelatesTo => {
                    has_support = true;
                    let src = &inner.graph[edge_ref.source()];
                    rationale_parts.push(format!(
                        "Grounded by {} ({}): {}",
                        src.agent_id,
                        session_short_name(&inner.session_order, &src.session_id),
                        edge.reasoning
                    ));
                }
                EdgeType::Refutes => {
                    has_contradiction = true;
                    let src = &inner.graph[edge_ref.source()];
                    rationale_parts.push(format!(
                        "Refuted by {} ({}): {}",
                        src.agent_id,
                        session_short_name(&inner.session_order, &src.session_id),
                        edge.reasoning
                    ));
                }
                EdgeType::Supersedes => {
                    is_superseded = true;
                    let src = &inner.graph[edge_ref.source()];
                    rationale_parts.push(format!(
                        "Superseded by {} ({}): {}",
                        src.agent_id,
                        session_short_name(&inner.session_order, &src.session_id),
                        edge.reasoning
                    ));
                }
                _ => {}
            }
        }

        let mut has_resolution = false;
        let mut validation_criteria: Vec<String> = Vec::new();
        let mut blocked_work: Vec<String> = Vec::new();

        for edge_ref in inner.graph.edges_directed(node_idx, Direction::Outgoing) {
            let edge = edge_ref.weight();
            let tgt = &inner.graph[edge_ref.target()];

            if edge.edge_type == EdgeType::Resolves {
                has_resolution = true;
                if tgt.node_type == NodeType::OpenItem {
                    validation_criteria.push(format!("Resolves open item: {}", tgt.label));
                }
            }

            if edge.edge_type == EdgeType::RelatesTo {
                blocked_work.push(tgt.label.clone());
            }
        }

        let mut downstream_labels = collect_downstream_supported(inner, node_idx, 2);
        blocked_work.append(&mut downstream_labels);
        blocked_work.sort();
        blocked_work.dedup();

        let status = if is_superseded {
            "Superseded".to_string()
        } else if has_support && has_resolution {
            "Validated".to_string()
        } else if has_contradiction && !has_resolution {
            "Blocked".to_string()
        } else {
            "Pending".to_string()
        };

        let rationale = if rationale_parts.is_empty() {
            format!(
                "Decision made by {} in {}: {}",
                node.agent_id,
                session_short_name(&inner.session_order, &node.session_id),
                node.description
            )
        } else {
            rationale_parts.join("; ")
        };

        let owner = if !node.agent_id.is_empty() {
            Some(node.agent_id.clone())
        } else {
            None
        };

        results.push(Decision {
            id: node.id.clone(),
            label: node.label.clone(),
            rationale,
            status,
            owner,
            session_id: node.session_id.clone(),
            timestamp: node.timestamp,
            blocked_work: blocked_work.into_iter().take(10).collect(),
            validation_criteria,
        });
    }

    results.sort_by(|a, b| b.timestamp.partial_cmp(&a.timestamp).unwrap_or(std::cmp::Ordering::Equal));
    results
}

fn collect_downstream_supported(
    inner: &GraphStoreInner,
    start: petgraph::stable_graph::NodeIndex<crate::store::GraphIx>,
    depth: usize,
) -> Vec<String> {
    let mut labels = Vec::new();
    let mut visited = HashSet::new();
    let mut queue = VecDeque::new();
    queue.push_back((start, 0));
    visited.insert(start);

    while let Some((idx, d)) = queue.pop_front() {
        if d >= depth {
            continue;
        }
        for edge_ref in inner.graph.edges_directed(idx, Direction::Outgoing) {
            let edge = edge_ref.weight();
            if edge.edge_type != EdgeType::RelatesTo {
                continue;
            }
            let tgt = edge_ref.target();
            if visited.insert(tgt) {
                let tgt_node = &inner.graph[tgt];
                if tgt_node.node_type == NodeType::OpenItem {
                    labels.push(tgt_node.label.clone());
                }
                queue.push_back((tgt, d + 1));
            }
        }
    }

    labels
}
