use std::collections::HashMap;

use crate::types::{ChangedNode, GraphDiff, GraphSnapshot};

/// Compute the diff between two graph snapshots.
///
/// Returns the nodes/edges that were added, removed, or changed
/// between `before` and `after`.
pub fn compute_graph_diff(before: &GraphSnapshot, after: &GraphSnapshot) -> GraphDiff {
    let before_nodes: HashMap<&str, _> = before.nodes.iter().map(|n| (n.id.as_str(), n)).collect();
    let after_nodes: HashMap<&str, _> = after.nodes.iter().map(|n| (n.id.as_str(), n)).collect();

    let before_edges: HashMap<&str, _> = before.edges.iter().map(|e| (e.id.as_str(), e)).collect();
    let after_edges: HashMap<&str, _> = after.edges.iter().map(|e| (e.id.as_str(), e)).collect();

    // ── Nodes ──────────────────────────────────────────────────────────────
    let added_nodes: Vec<_> = after
        .nodes
        .iter()
        .filter(|n| !before_nodes.contains_key(n.id.as_str()))
        .cloned()
        .collect();

    let removed_nodes: Vec<_> = before
        .nodes
        .iter()
        .filter(|n| !after_nodes.contains_key(n.id.as_str()))
        .cloned()
        .collect();

    // ── Changed nodes ──────────────────────────────────────────────────────
    let mut changed = Vec::new();
    for (&id, after_node) in &after_nodes {
        if let Some(before_node) = before_nodes.get(id) {
            let mut fields = Vec::new();
            if before_node.label != after_node.label {
                fields.push("label".to_string());
            }
            if before_node.description != after_node.description {
                fields.push("description".to_string());
            }
            if before_node.node_type != after_node.node_type {
                fields.push("node_type".to_string());
            }
            if before_node.agent_id != after_node.agent_id {
                fields.push("agent_id".to_string());
            }
            if (before_node.confidence - after_node.confidence).abs() > 0.01 {
                fields.push("confidence".to_string());
            }
            if !fields.is_empty() {
                changed.push(ChangedNode {
                    node_id: id.to_string(),
                    fields,
                });
            }
        }
    }

    // ── Edges ──────────────────────────────────────────────────────────────
    let added_edges: Vec<_> = after
        .edges
        .iter()
        .filter(|e| !before_edges.contains_key(e.id.as_str()))
        .cloned()
        .collect();

    let removed_edges: Vec<_> = before
        .edges
        .iter()
        .filter(|e| !after_edges.contains_key(e.id.as_str()))
        .cloned()
        .collect();

    GraphDiff {
        added: GraphSnapshot {
            nodes: added_nodes,
            edges: added_edges,
        },
        removed: GraphSnapshot {
            nodes: removed_nodes,
            edges: removed_edges,
        },
        changed,
    }
}
