use petgraph::Direction;

use crate::store::GraphStoreInner;
use crate::types::{DebtItem, DebtReport, EdgeType, NodeStatus, NodeType};
use crate::utils::session_index;

fn severity_score(severity: &str) -> u32 {
    match severity {
        "High" => 3,
        "Medium" => 2,
        _ => 1,
    }
}

/// Graph-internal debt (v2 minimal taxonomy).
///
/// With only Decision/OpenItem nodes, unresolved open items are the one
/// graph-internal debt signal. The higher-value detectors
/// (`UnverifiedChange`, `FailedVerification`) are cross-layer: they compare
/// session activity against this graph and arrive with the scratch layer.
pub fn compute_debt(
    inner: &GraphStoreInner,
    current_session_idx: usize,
    timestamp: f64,
) -> DebtReport {
    let mut items: Vec<DebtItem> = Vec::new();

    // Unresolved OpenItems: no incoming Resolves edge.
    for idx in inner.graph.node_indices() {
        let node = &inner.graph[idx];
        if node.node_type != NodeType::OpenItem {
            continue;
        }
        if node.status == NodeStatus::Invalidated {
            continue;
        }
        let has_incoming_resolves = inner
            .graph
            .edges_directed(idx, Direction::Incoming)
            .any(|e| e.weight().edge_type == EdgeType::Resolves);
        if !has_incoming_resolves {
            let age = session_index(&inner.session_order, &node.session_id)
                .map(|m| current_session_idx.saturating_sub(m))
                .unwrap_or(0);
            let severity = if age >= 1 { "High" } else { "Medium" };
            items.push(DebtItem {
                debt_type: "UnresolvedOpenItem".to_string(),
                node_ids: vec![node.id.clone()],
                description: format!(
                    "Open item '{}' raised by {} has not been resolved",
                    node.label, node.agent_id
                ),
                severity: severity.to_string(),
            });
        }
    }

    let total_score: u32 = items.iter().map(|i| severity_score(&i.severity)).sum();

    DebtReport {
        items,
        total_score,
        timestamp,
    }
}
