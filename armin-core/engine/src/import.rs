//! Deterministic import of project docs (AGENTS.md / CLAUDE.md) into the
//! durable graph.
//!
//! This is the cold-start path: an existing project usually has its
//! conventions and constraints written down already, and importing them
//! makes the very first session's brief useful — no extraction, no LLM,
//! no network. Parsing is deliberately conservative heuristics over
//! markdown lines; everything becomes a project-wide node (no file scope).
//!
//! Node IDs are content hashes of the normalized text, so re-importing the
//! same document is a no-op (GraphStore dedupes by node ID).

use armin_graph::{ArgumentNode, NodeStatus, NodeType};
use sha2::{Digest, Sha256};

/// A parsed import candidate before dedup.
#[derive(Clone, Debug, PartialEq)]
pub struct ImportNode {
    pub node_type: NodeType,
    pub label: String,
    pub description: String,
}

/// Classify one markdown content line into a durable node, or None.
pub fn classify_line(line: &str) -> Option<ImportNode> {
    let text = line
        .trim()
        .trim_start_matches(['-', '*', '+', '>'])
        .trim_start_matches(|c: char| c.is_ascii_digit() || c == '.' || c == ')')
        .trim()
        .trim_end_matches(['.', ';'])
        .to_string();
    // Strip markdown emphasis for the text.
    let text = text.replace(['*', '_', '`'], "");
    if text.len() < 8 {
        return None;
    }
    let lowered = text.to_lowercase();
    // Skip code fences and table rows.
    if text.starts_with('|') || text.starts_with("```") || text.starts_with('#') {
        return None;
    }

    let is_question = lowered.contains('?')
        || lowered.starts_with("todo")
        || lowered.starts_with("tbd")
        || lowered.starts_with("open question");
    if is_question {
        return Some(make_node(NodeType::OpenItem, &text));
    }

    const RULE_MARKERS: &[&str] = &[
        "must", "must not", "never", "always", "do not", "don't ", "avoid ",
        "required", "has to", "only use", "no third-party", "forbidden",
        "prohibited", "should be", "keep ", "use ", "prefer ", "make sure",
        "ensure ", "needs to", "mandatory", "standard library only",
    ];
    const DECISION_MARKERS: &[&str] = &[
        "we use", "we chose", "we decided", "we do", "uses ", "instead of",
        "over ", "chosen", "decided", "settled", "picked", "committed to",
        "the project uses", "convention:", "pattern:", "architecture:",
    ];

    // Decision markers are more specific than the generic imperative rule
    // markers ("we use X" is a decision even though it contains "use").
    if DECISION_MARKERS.iter().any(|m| lowered.contains(m)) {
        Some(make_node(NodeType::Decision, &text))
    } else if RULE_MARKERS.iter().any(|m| lowered.contains(m)) {
        Some(make_node(NodeType::Rule, &text))
    } else {
        None
    }
}

fn make_node(node_type: NodeType, text: &str) -> ImportNode {
    let words: Vec<&str> = text.split_whitespace().collect();
    let label = if words.len() > 12 {
        format!("{}…", words[..12].join(" "))
    } else {
        text.to_string()
    };
    ImportNode {
        node_type,
        label,
        description: text.to_string(),
    }
}

fn stable_id(text: &str) -> String {
    let normalized: String = text
        .to_lowercase()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || c.is_whitespace())
        .collect();
    let hash = Sha256::digest(normalized.trim());
    let hex: String = hash[..8].iter().map(|b| format!("{b:02x}")).collect();
    format!("imp-{hex}")
}

/// Parse a full markdown document.
pub fn parse_markdown(content: &str) -> Vec<ImportNode> {
    let mut out = Vec::new();
    let mut in_code_fence = false;
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("```") {
            in_code_fence = !in_code_fence;
            continue;
        }
        if in_code_fence {
            continue;
        }
        // Headings and plain prose lines are candidates too ("# Storage:
        // use SQLite" style), but only bullet-like lines are usually
        // constraints — accept both via classify_line's trimming.
        if trimmed.is_empty() {
            continue;
        }
        // Only bullets and numbered items; headings carry section context we
        // do not model yet.
        let is_list_item = trimmed.starts_with(['-', '*', '+', '>'])
            || trimmed
                .chars()
                .next()
                .map(|c| c.is_ascii_digit())
                .unwrap_or(false);
        if !is_list_item {
            continue;
        }
        if let Some(node) = classify_line(trimmed) {
            out.push(node);
        }
    }
    out
}

/// Build concrete ArgumentNodes (content-hash IDs, project-wide scope).
pub fn to_argument_nodes(
    parsed: &[ImportNode],
    session_id: &str,
    timestamp: f64,
) -> Vec<ArgumentNode> {
    parsed
        .iter()
        .map(|n| ArgumentNode {
            id: stable_id(&n.description),
            node_type: n.node_type.clone(),
            label: n.label.clone(),
            description: n.description.clone(),
            event_id: format!("{}-import", session_id),
            agent_id: "import".to_string(),
            session_id: session_id.to_string(),
            timestamp,
            confidence: 1.0,
            files: vec![],
            commit: None,
            mention_count: 1,
            status: NodeStatus::Active,
        })
        .collect()
}

#[derive(serde::Deserialize)]
pub struct ImportRequest {
    pub content: String,
    #[serde(default)]
    pub session_id: Option<String>,
}

#[derive(serde::Serialize)]
pub struct ImportResponse {
    pub found: usize,
    pub imported: usize,
    pub rules: usize,
    pub decisions: usize,
    pub open_items: usize,
}

/// Handle an import request: parse, build nodes, insert (idempotent).
pub async fn handle_import(
    graph: &armin_graph::GraphStore,
    req: ImportRequest,
) -> ImportResponse {
    let parsed = parse_markdown(&req.content);
    let session_id = req.session_id.unwrap_or_else(|| "import".to_string());
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64();
    let nodes = to_argument_nodes(&parsed, &session_id, ts);
    let mut imported = 0usize;
    for node in nodes {
        // add_node dedupes by ID — re-import is a no-op.
        let existed = graph.get_node(&node.id).await.is_some();
        graph.add_node(node).await;
        if !existed {
            imported += 1;
        }
    }
    ImportResponse {
        found: parsed.len(),
        imported,
        rules: parsed.iter().filter(|n| n.node_type == NodeType::Rule).count(),
        decisions: parsed
            .iter()
            .filter(|n| n.node_type == NodeType::Decision)
            .count(),
        open_items: parsed
            .iter()
            .filter(|n| n.node_type == NodeType::OpenItem)
            .count(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_constraints_questions_and_decisions() {
        let r = classify_line("- You must never print timestamps in CLI output").unwrap();
        assert_eq!(r.node_type, NodeType::Rule);

        let r = classify_line("* Standard library only; no third-party dependencies").unwrap();
        assert_eq!(r.node_type, NodeType::Rule);

        let r = classify_line("- We use SQLite for persistence").unwrap();
        assert_eq!(r.node_type, NodeType::Decision);

        let r = classify_line("- TODO: add retry logic for the export path?").unwrap();
        assert_eq!(r.node_type, NodeType::OpenItem);

        assert!(classify_line("- Thanks for the review!").is_none());
        assert!(classify_line("- See the docs folder").is_none());
    }

    #[test]
    fn parse_is_idempotent_by_content_hash() {
        let doc = "# Project rules\n- All data files must use UTF-8\n- Use serde for serialization\n";
        let a = to_argument_nodes(&parse_markdown(doc), "s1", 1.0);
        let b = to_argument_nodes(&parse_markdown(doc), "s2", 2.0);
        assert_eq!(a.len(), 2);
        assert_eq!(a[0].id, b[0].id);
    }

    #[test]
    fn skips_code_fences() {
        let doc = "- Real rule: never commit secrets\n```\n- ignore: must be ignored\n```\n";
        let parsed = parse_markdown(doc);
        assert_eq!(parsed.len(), 1);
    }
}
