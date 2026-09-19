use std::collections::{HashMap, HashSet};

use armin_graph::{ArgumentEdge, ArgumentNode, EdgeType, GraphSnapshot};

pub fn build_template_answer(trace: &[String], subgraph: &GraphSnapshot) -> String {
    if trace.is_empty() {
        return "No relevant nodes found in the graph.".to_string();
    }

    let node_map: HashMap<&str, &ArgumentNode> =
        subgraph.nodes.iter().map(|n| (n.id.as_str(), n)).collect();

    let limited_trace: Vec<String> = trace.iter().take(12).cloned().collect();
    let limited_set: HashSet<&str> = limited_trace.iter().map(|s| s.as_str()).collect();

    let mut sections: Vec<String> = Vec::new();

    let mut decisions: Vec<&ArgumentNode> = Vec::new();
    let mut rules: Vec<&ArgumentNode> = Vec::new();
    let mut open_items: Vec<&ArgumentNode> = Vec::new();

    for node_id in &limited_trace {
        if let Some(node) = node_map.get(node_id.as_str()) {
            match node.node_type {
                armin_graph::NodeType::Decision => decisions.push(node),
                armin_graph::NodeType::Rule => rules.push(node),
                armin_graph::NodeType::OpenItem => open_items.push(node),
            }
        }
    }

    let mut edge_set: HashSet<(&str, &str)> = HashSet::new();
    let mut outgoing: HashMap<&str, Vec<(&ArgumentNode, &ArgumentEdge)>> = HashMap::new();
    for edge in &subgraph.edges {
        if limited_set.contains(edge.source_node_id.as_str())
            && limited_set.contains(edge.target_node_id.as_str())
        {
            let key = (edge.source_node_id.as_str(), edge.target_node_id.as_str());
            if edge_set.insert(key) {
                if let Some(tgt) = node_map.get(edge.target_node_id.as_str()) {
                    outgoing.entry(edge.source_node_id.as_str()).or_default().push((tgt, edge));
                }
            }
        }
    }

    for edges in outgoing.values_mut() {
        edges.sort_by_key(|(_, e)| e.edge_type.preference_order());
        edges.truncate(3);
    }

    let fmt_node = |node: &ArgumentNode| -> String {
        format!("**{}** ({}) {}", node.agent_id, node.session_id, node.label)
    };

    let edge_label = |et: &EdgeType| -> &str {
        match et {
            EdgeType::Supersedes => "supersedes",
            EdgeType::Refutes => "refutes",
            EdgeType::RelatesTo => "relates to",
            EdgeType::Resolves => "resolves",
        }
    };

    let render_section = |title: &str, nodes: &[&ArgumentNode]| -> Option<String> {
        if nodes.is_empty() {
            return None;
        }
        let mut lines = vec![format!("### {}", title)];
        for node in nodes {
            let mut line = format!("- {}", fmt_node(node));
            if let Some(edges) = outgoing.get(node.id.as_str()) {
                for (target, edge) in edges {
                    line.push_str(&format!("\n  - *{}* → {}", edge_label(&edge.edge_type), target.label));
                }
            }
            lines.push(line);
        }
        Some(lines.join("\n"))
    };

    if let Some(s) = render_section("Decisions", &decisions) {
        sections.push(s);
    }
    if let Some(s) = render_section("Rules", &rules) {
        sections.push(s);
    }

    let chains = extract_reasoning_chains(&limited_trace, subgraph, &node_map);
    if !chains.is_empty() {
        let mut lines = vec!["### Reasoning Chains".to_string()];
        for (i, chain) in chains.iter().enumerate().take(5) {
            let chain_str: Vec<String> = chain
                .iter()
                .map(|(node, edge_opt)| {
                    let short = format!("{} ({})", node.agent_id, node.session_id);
                    if let Some(edge) = edge_opt {
                        format!("{} → *{}*", short, edge_label(&edge.edge_type))
                    } else {
                        short
                    }
                })
                .collect();
            lines.push(format!("{}. {}", i + 1, chain_str.join(" ")));
        }
        sections.push(lines.join("\n"));
    }

    if let Some(s) = render_section("Open Items", &open_items) {
        sections.push(s);
    }

    if sections.is_empty() {
        return "No relevant nodes found in the graph.".to_string();
    }

    sections.join("\n\n")
}

fn extract_reasoning_chains<'a>(
    trace: &'a [String],
    subgraph: &'a GraphSnapshot,
    node_map: &'a HashMap<&str, &ArgumentNode>,
) -> Vec<Vec<(&'a ArgumentNode, Option<&'a ArgumentEdge>)>> {
    let mut edges_from: HashMap<&str, Vec<(&str, &ArgumentEdge)>> = HashMap::new();
    for edge in &subgraph.edges {
        edges_from
            .entry(edge.source_node_id.as_str())
            .or_default()
            .push((edge.target_node_id.as_str(), edge));
    }

    let trace_set: HashSet<&str> = trace.iter().map(|s| s.as_str()).collect();
    let mut has_incoming: HashSet<&str> = HashSet::new();
    for edge in &subgraph.edges {
        if trace_set.contains(edge.source_node_id.as_str())
            && trace_set.contains(edge.target_node_id.as_str())
        {
            has_incoming.insert(edge.target_node_id.as_str());
        }
    }

    let mut chains: Vec<Vec<(&ArgumentNode, Option<&ArgumentEdge>)>> = Vec::new();
    let mut visited: HashSet<&str> = HashSet::new();

    let roots: Vec<&str> = trace
        .iter()
        .map(|s| s.as_str())
        .filter(|id| !has_incoming.contains(id))
        .collect();

    for root_id in roots {
        if visited.contains(root_id) {
            continue;
        }
        let mut chain: Vec<(&ArgumentNode, Option<&ArgumentEdge>)> = Vec::new();
        let mut current = root_id;

        loop {
            if let Some(node) = node_map.get(current) {
                if visited.contains(current) {
                    break;
                }
                visited.insert(current);
                let edge_opt = if let Some(last) = chain.last() {
                    edges_from
                        .get(last.0.id.as_str())
                        .and_then(|edges| {
                            edges.iter().find(|(tgt, _)| *tgt == current).map(|(_, e)| *e)
                        })
                } else {
                    None
                };
                chain.push((node, edge_opt));

                if let Some(neighbors) = edges_from.get(current) {
                    let next = neighbors
                        .iter()
                        .find(|(tgt, _)| trace_set.contains(tgt) && !visited.contains(tgt));
                    if let Some((next_id, _)) = next {
                        current = next_id;
                        continue;
                    }
                }
            }
            break;
        }

        if chain.len() >= 2 && chain.len() <= 6 {
            chains.push(chain);
        }
    }

    chains
}
