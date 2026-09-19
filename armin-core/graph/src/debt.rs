use std::collections::{HashMap, HashSet};

use petgraph::Direction;

use crate::store::GraphStoreInner;
use crate::types::{DebtItem, DebtReport, EdgeType, NodeStatus, NodeType, ScratchSnapshot};
use crate::utils::session_index;

fn severity_score(severity: &str) -> u32 {
    match severity {
        "High" => 3,
        "Medium" => 2,
        _ => 1,
    }
}

fn files_overlap(a: &[String], b: &[String]) -> bool {
    a.iter().any(|f| b.contains(f))
}

/// Graph-internal + cross-layer debt (v2 taxonomy).
///
/// `scratch` carries the session's recent mutations and verification
/// outcomes. The graph itself stays durable-only; the highest-value
/// detectors are tensions BETWEEN session activity and this graph:
///
/// - `UnverifiedChange`   — a mutation with no subsequent check on its files
/// - `FailedVerification` — the latest check on files failed, nothing fixed it since
/// - `RuleViolation`      — a failed check on files covered by an active Rule
/// - `UnresolvedOpenItem` — graph-internal: an open item with no Resolves edge
pub fn compute_debt(
    inner: &GraphStoreInner,
    current_session_idx: usize,
    timestamp: f64,
    scratch: Option<&ScratchSnapshot>,
) -> DebtReport {
    let mut items: Vec<DebtItem> = Vec::new();

    // 1. Unresolved OpenItems: no incoming Resolves edge.
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

    let Some(scratch) = scratch else {
        let total_score: u32 = items.iter().map(|i| severity_score(&i.severity)).sum();
        return DebtReport {
            items,
            total_score,
            timestamp,
        };
    };

    // 2. UnverifiedChange: an edit whose files were never checked afterwards.
    for edit in &scratch.edits {
        if edit.files.is_empty() {
            continue;
        }
        let verified = scratch
            .checks
            .iter()
            .any(|c| c.timestamp >= edit.timestamp && files_overlap(&c.files, &edit.files));
        if !verified {
            let files = edit
                .files
                .iter()
                .take(3)
                .cloned()
                .collect::<Vec<_>>()
                .join(", ");
            items.push(DebtItem {
                debt_type: "UnverifiedChange".to_string(),
                node_ids: vec![edit.event_id.clone()],
                description: format!(
                    "Edited '{}' via {} — no test/lint/build run covered it afterwards",
                    files, edit.tool
                ),
                severity: "Medium".to_string(),
            });
        }
    }

    // 3. FailedVerification: the latest check on a file failed and no later
    //    edit attempted a fix.
    let mut latest_check: HashMap<&str, &crate::types::ScratchCheck> = HashMap::new();
    for check in &scratch.checks {
        for f in &check.files {
            match latest_check.get(f.as_str()) {
                Some(prev) if prev.timestamp >= check.timestamp => {}
                _ => {
                    latest_check.insert(f.as_str(), check);
                }
            }
        }
    }
    let mut reported: HashSet<&str> = HashSet::new();
    for check in latest_check.values() {
        if check.passed || !reported.insert(check.event_id.as_str()) {
            continue;
        }
        let fixed = scratch
            .edits
            .iter()
            .any(|e| e.timestamp >= check.timestamp && files_overlap(&e.files, &check.files));
        if fixed {
            continue;
        }
        let files = check
            .files
            .iter()
            .take(3)
            .cloned()
            .collect::<Vec<_>>()
            .join(", ");
        items.push(DebtItem {
            debt_type: "FailedVerification".to_string(),
            node_ids: vec![check.event_id.clone()],
            description: format!(
                "Check '{}' on '{}' failed — no fix attempted since",
                check.tool, files
            ),
            severity: "High".to_string(),
        });
    }

    // 4. RuleViolation (best effort): a failed check on files covered by an
    //    active Rule. The rule may be the cause — flag for attention.
    let rules: Vec<&crate::types::ArgumentNode> = inner
        .graph
        .node_indices()
        .filter(|&i| {
            inner.graph[i].node_type == NodeType::Rule && inner.graph[i].status == NodeStatus::Active
        })
        .map(|i| &inner.graph[i])
        .collect();
    for check in &scratch.checks {
        if check.passed || check.files.is_empty() {
            continue;
        }
        for rule in &rules {
            if !rule.files.is_empty() && files_overlap(&rule.files, &check.files) {
                items.push(DebtItem {
                    debt_type: "RuleViolation".to_string(),
                    node_ids: vec![rule.id.clone(), check.event_id.clone()],
                    description: format!(
                        "Check '{}' failed on files under rule '{}' — possible violation",
                        check.tool, rule.label
                    ),
                    severity: "High".to_string(),
                });
                break; // one item per failing check
            }
        }
    }

    let total_score: u32 = items.iter().map(|i| severity_score(&i.severity)).sum();

    DebtReport {
        items,
        total_score,
        timestamp,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::GraphStore;
    use crate::types::ScratchCheck;

    #[tokio::test]
    async fn unverified_edit_is_flagged_and_cleared_by_check() {
        let store = GraphStore::new();
        let edit = crate::types::ScratchEdit {
            event_id: "e1".into(),
            tool: "edit".into(),
            files: vec!["src/main.rs".into()],
            timestamp: 10.0,
        };
        let scratch = ScratchSnapshot { edits: vec![edit], checks: vec![] };
        let report = store.compute_debt_report_with(0, Some(&scratch)).await;
        assert!(report.items.iter().any(|i| i.debt_type == "UnverifiedChange"));

        let scratch = ScratchSnapshot {
            edits: scratch.edits,
            checks: vec![ScratchCheck {
                event_id: "c1".into(),
                tool: "test".into(),
                files: vec!["src/main.rs".into()],
                passed: true,
                timestamp: 12.0,
            }],
        };
        let report = store.compute_debt_report_with(0, Some(&scratch)).await;
        assert!(!report.items.iter().any(|i| i.debt_type == "UnverifiedChange"));
    }

    #[tokio::test]
    async fn failed_check_flags_until_fixed() {
        let store = GraphStore::new();
        let check = ScratchCheck {
            event_id: "c1".into(),
            tool: "pytest".into(),
            files: vec!["src/lib.rs".into()],
            passed: false,
            timestamp: 10.0,
        };
        let scratch = ScratchSnapshot { edits: vec![], checks: vec![check.clone()] };
        let report = store.compute_debt_report_with(0, Some(&scratch)).await;
        assert!(report.items.iter().any(|i| i.debt_type == "FailedVerification"));

        // A later edit counts as attempting a fix.
        let scratch = ScratchSnapshot {
            edits: vec![crate::types::ScratchEdit {
                event_id: "e9".into(),
                tool: "edit".into(),
                files: vec!["src/lib.rs".into()],
                timestamp: 11.0,
            }],
            checks: vec![check],
        };
        let report = store.compute_debt_report_with(0, Some(&scratch)).await;
        assert!(!report.items.iter().any(|i| i.debt_type == "FailedVerification"));
    }

    #[tokio::test]
    async fn rule_violation_needs_scoped_rule_and_failed_check() {
        let store = GraphStore::new();
        store
            .add_node(crate::types::ArgumentNode {
                id: "r1".into(),
                node_type: NodeType::Rule,
                label: "Never print timestamps".into(),
                description: "binding".into(),
                event_id: "e0".into(),
                agent_id: "user".into(),
                session_id: "s1".into(),
                timestamp: 1.0,
                confidence: 1.0,
                files: vec!["src/cli.rs".into()],
                commit: None,
                mention_count: 1,
                status: NodeStatus::Active,
            })
            .await;
        let scratch = ScratchSnapshot {
            edits: vec![],
            checks: vec![ScratchCheck {
                event_id: "c1".into(),
                tool: "pytest".into(),
                files: vec!["src/cli.rs".into()],
                passed: false,
                timestamp: 10.0,
            }],
        };
        let report = store.compute_debt_report_with(0, Some(&scratch)).await;
        assert!(report.items.iter().any(|i| i.debt_type == "RuleViolation"));
    }
}
