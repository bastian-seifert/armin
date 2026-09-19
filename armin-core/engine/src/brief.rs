//! Compact reasoning-state brief for system-reminder injection.
//!
//! The embedding harness appends this to the agent's system prompt on every
//! turn. It is built from precomputed graph analytics (no LLM calls), capped
//! to a few hundred tokens, and returns an empty string when the graph has
//! nothing useful to say so the plugin can skip injection entirely.



use crate::state::EngineState;

/// Maximum number of list items per section.
const MAX_ITEMS: usize = 3;
/// Decision sections may carry more: constraints stated mid-session must
/// not be cut by the cap (first-3 was all instruction echoes in practice).
const MAX_DECISIONS: usize = 6;
/// Maximum characters of any single label/rationale.
const MAX_TEXT: usize = 110;

pub async fn build_brief(state: &EngineState, active_files: &[String]) -> String {
    let node_count = state.graph.node_count().await;
    if node_count == 0 {
        return String::new();
    }

    let session_idx = state
        .current_session_idx
        .load(std::sync::atomic::Ordering::SeqCst);
    let session_id = state.current_session_id().await;
    let scratch = state.scratch.snapshot(&session_id).await;
    let debt = state
        .graph
        .compute_debt_report_with(session_idx, Some(&scratch))
        .await;
    let decisions = state.graph.extract_decisions(session_idx).await;
    let edges = state.graph.edge_count().await;

    let mut sections: Vec<String> = Vec::new();

    // Decisions — decided ones first, then the most recent pending ones.
    // Ordering by recency matters: constraints stated late in a session are
    // typically the binding ones for follow-up work.
    let mut chosen: Vec<&armin_graph::Decision> = decisions
        .iter()
        .filter(|d| d.status != "Pending")
        .collect();
    for d in decisions.iter().rev() {
        if chosen.len() >= MAX_DECISIONS {
            break;
        }
        if !chosen.iter().any(|c| c.label == d.label) {
            chosen.push(d);
        }
    }
    chosen.truncate(MAX_DECISIONS);
    if !chosen.is_empty() {
        let lines: Vec<String> = chosen
            .iter()
            .map(|d| format!("- [{}] {}", d.status, truncate(&d.label, MAX_TEXT)))
            .collect();
        sections.push(format!(
            "Decisions ({total}):\n{lines}",
            total = decisions.len(),
            lines = lines.join("\n")
        ));
    }

    // Rules — binding constraints, always worth surfacing (they are few:
    // imported or recorded, never prose-extracted).
    let rules: Vec<armin_graph::ArgumentNode> = snapshot_nodes(state)
        .await
        .into_iter()
        .filter(|n| n.node_type == armin_graph::NodeType::Rule)
        .take(MAX_ITEMS)
        .collect();
    if !rules.is_empty() {
        let lines: Vec<String> = rules
            .iter()
            .map(|n| format!("- {}", truncate(&n.label, MAX_TEXT)))
            .collect();
        sections.push(format!("Rules ({}):\n{}", rules.len(), lines.join("\n")));
    }

    // Debt — open items are the one graph-internal debt signal.
    let open_items: Vec<&armin_graph::DebtItem> = debt
        .items
        .iter()
        .filter(|i| i.debt_type.eq_ignore_ascii_case("UnresolvedOpenItem"))
        .collect();
    if !open_items.is_empty() {
        let lines: Vec<String> = open_items
            .iter()
            .take(MAX_ITEMS)
            .map(|i| format!("- {}", truncate(&i.description, MAX_TEXT)))
            .collect();
        sections.push(format!(
            "Open items ({}):\n{}",
            open_items.len(),
            lines.join("\n")
        ));
    }

    // Binding here: rules/decisions scoped to the files the agent is
    // working on right now (nodes with file scope that intersect the
    // request). Project-wide nodes (no files) live in the sections above.
    if !active_files.is_empty() {
        let snapshot = state.graph.snapshot().await;
        let mut binding: Vec<&armin_graph::ArgumentNode> = snapshot
            .nodes
            .iter()
            .filter(|n| {
                !n.files.is_empty()
                    && matches!(n.node_type, armin_graph::NodeType::Rule | armin_graph::NodeType::Decision)
                    && n.files.iter().any(|f| {
                        active_files.iter().any(|a| a == f || f.ends_with(a) || a.ends_with(f))
                    })
            })
            .collect();
        binding.sort_by_key(|n| match n.node_type {
            armin_graph::NodeType::Rule => 0,
            _ => 1,
        });
        binding.truncate(5);
        if !binding.is_empty() {
            let lines: Vec<String> = binding
                .iter()
                .map(|n| {
                    format!(
                        "- [{}] {}",
                        if n.node_type == armin_graph::NodeType::Rule { "Rule" } else { "Decision" },
                        truncate(&n.label, MAX_TEXT)
                    )
                })
                .collect();
            sections.push(format!(
                "Binding here ({}):\n{}",
                binding.len(),
                lines.join("\n")
            ));
        }
    }

    // Cross-layer warnings: session activity vs the durable graph.
    let is = |item: &armin_graph::DebtItem, kind: &str| item.debt_type.eq_ignore_ascii_case(kind);
    let unverified: Vec<&armin_graph::DebtItem> = debt
        .items
        .iter()
        .filter(|i| is(i, "UnverifiedChange"))
        .collect();
    if !unverified.is_empty() {
        let lines: Vec<String> = unverified
            .iter()
            .take(MAX_ITEMS)
            .map(|i| format!("- {}", truncate(&i.description, MAX_TEXT)))
            .collect();
        sections.push(format!(
            "Unverified edits ({}):\n{}",
            unverified.len(),
            lines.join("\n")
        ));
    }

    let failing: Vec<&armin_graph::DebtItem> = debt
        .items
        .iter()
        .filter(|i| is(i, "FailedVerification") || is(i, "RuleViolation"))
        .collect();
    if !failing.is_empty() {
        let lines: Vec<String> = failing
            .iter()
            .take(2)
            .map(|i| format!("- {}", truncate(&i.description, MAX_TEXT)))
            .collect();
        sections.push(format!(
            "Failing checks ({}):\n{}",
            failing.len(),
            lines.join("\n")
        ));
    }

    if sections.is_empty() {
        return String::new();
    }

    format!(
        "<reasoning-state nodes=\"{node_count}\" edges=\"{edges}\">\n{}\n</reasoning-state>",
        sections.join("\n")
    )
}

async fn snapshot_nodes(state: &EngineState) -> Vec<armin_graph::ArgumentNode> {
    state.graph.snapshot().await.nodes
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let cut: String = s.chars().take(max).collect();
    format!("{cut}…")
}
