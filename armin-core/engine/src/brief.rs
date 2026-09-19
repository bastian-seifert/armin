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

pub async fn build_brief(state: &EngineState) -> String {
    let node_count = state.graph.node_count().await;
    if node_count == 0 {
        return String::new();
    }

    let session_idx = state
        .current_session_idx
        .load(std::sync::atomic::Ordering::SeqCst);
    let debt = state.graph.compute_debt_report(session_idx).await;
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

    if sections.is_empty() {
        return String::new();
    }

    format!(
        "<reasoning-state nodes=\"{node_count}\" edges=\"{edges}\">\n{}\n</reasoning-state>",
        sections.join("\n")
    )
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let cut: String = s.chars().take(max).collect();
    format!("{cut}…")
}
