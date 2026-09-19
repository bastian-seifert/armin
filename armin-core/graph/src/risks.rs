use std::collections::{HashMap, HashSet, VecDeque};

use petgraph::visit::EdgeRef;
use petgraph::Direction;

use crate::debt::compute_debt;
use crate::store::GraphStoreInner;
use crate::types::Risk;
use crate::utils::session_index;

/// Compute risks from the current graph state.
///
/// Seeds from debt items (orphan assumptions, unsupported claims, active contradictions)
/// and scores each by downstream impact and recency.
pub fn compute_risks(
    inner: &GraphStoreInner,
    current_session_idx: usize,
) -> Vec<Risk> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64();

    let debt = compute_debt(inner, current_session_idx, now, None);
    let mut seen_nodes = HashSet::new();
    let mut results = Vec::new();

    // Precompute downstream counts for all nodes (cached)
    let downstream_counts = compute_downstream_counts(inner);

    for item in &debt.items {
        for node_id in &item.node_ids {
            if !seen_nodes.insert(node_id.clone()) {
                continue;
            }
            let Some(&node_idx) = inner.id_to_node.get(node_id) else {
                continue;
            };
            let node = &inner.graph[node_idx];

            // Impact = downstream count (capped) x session recency
            let downstream = *downstream_counts.get(node_id).unwrap_or(&0);
            let recency_bonus = match session_index(&inner.session_order, &node.session_id) {
                Some(si) if si >= current_session_idx.saturating_sub(1) => 20,
                Some(_) => 5,
                None => 0,
            };
            let impact = ((downstream as f32).min(4.0) / 4.0 * 60.0 + recency_bonus as f32)
                .clamp(5.0, 100.0);

            let validation_status = match item.debt_type.as_str() {
                "ActiveContradiction" => "Unvalidated",
                "OrphanAssumption" => "Unvalidated",
                "UnsupportedClaim" => "Partial",
                _ => "Unvalidated",
            };

            results.push(Risk {
                node_id: node.id.clone(),
                label: node.label.clone(),
                impact_score: impact.round(),
                validation_status: validation_status.to_string(),
                debt_type: item.debt_type.clone(),
                session_id: node.session_id.clone(),
            });
        }
    }

    // Sort by impact descending
    results.sort_by(|a, b| b.impact_score.partial_cmp(&a.impact_score).unwrap_or(std::cmp::Ordering::Equal));
    results
}

/// Compute total downstream nodes (via any outgoing edge) for each node.
fn compute_downstream_counts(inner: &GraphStoreInner) -> HashMap<String, usize> {
    let mut counts = HashMap::new();
    let all_nodes: Vec<_> = inner.graph.node_indices().collect();

    for &idx in &all_nodes {
        let id = inner.graph[idx].id.clone();
        let count = bfs_downstream_count(inner, idx, 3);
        counts.insert(id, count);
    }

    counts
}

fn bfs_downstream_count(
    inner: &GraphStoreInner,
    start: petgraph::stable_graph::NodeIndex<crate::store::GraphIx>,
    depth: usize,
) -> usize {
    let mut visited = HashSet::new();
    let mut queue = VecDeque::new();
    queue.push_back((start, 0));
    visited.insert(start);
    let mut count = 0;

    while let Some((idx, d)) = queue.pop_front() {
        if d >= depth {
            continue;
        }
        for edge_idx in inner.graph.edges_directed(idx, Direction::Outgoing) {
            let tgt = edge_idx.target();
            if visited.insert(tgt) {
                count += 1;
                queue.push_back((tgt, d + 1));
            }
        }
    }

    count
}
