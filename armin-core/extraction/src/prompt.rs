use std::sync::LazyLock;

use minijinja::{Environment, context};
use armin_graph::{ArgumentEdge, ArgumentNode};
use serde_json::{Value, json};

static ENV: LazyLock<Environment<'static>> = LazyLock::new(|| {
    let mut env = Environment::new();
    env.add_template_owned(
        "system_extraction",
        include_str!("prompts/system_extraction.jinja"),
    )
    .unwrap();
    env.add_template_owned(
        "user_extraction",
        include_str!("prompts/user_extraction.jinja"),
    )
    .unwrap();
    env.add_template_owned(
        "system_batch_extraction",
        include_str!("prompts/system_batch_extraction.jinja"),
    )
    .unwrap();
    env.add_template_owned(
        "user_batch_extraction",
        include_str!("prompts/user_batch_extraction.jinja"),
    )
    .unwrap();
    env.add_template_owned(
        "user_find_nodes",
        include_str!("prompts/user_find_nodes.jinja"),
    )
    .unwrap();
    env.add_template_owned(
        "user_answer_query",
        include_str!("prompts/user_answer_query.jinja"),
    )
    .unwrap();
    env
});

pub fn render_system_extraction() -> String {
    ENV.get_template("system_extraction").unwrap().render(context!()).unwrap()
}

pub fn render_system_batch_extraction() -> String {
    ENV.get_template("system_batch_extraction").unwrap().render(context!()).unwrap()
}

pub fn render_user_batch_extraction(
    recent_nodes: &[ArgumentNode],
    all_edges: &[ArgumentEdge],
    event_history: &[armin_ingest::EventRecord],
    new_events: &[armin_ingest::EventRecord],
    compressed_history: Option<&str>,
) -> String {
    let node_label: std::collections::HashMap<&str, &str> = recent_nodes
        .iter()
        .map(|n| (n.id.as_str(), n.label.as_str()))
        .collect();

    let nodes: Vec<_> = recent_nodes.iter().take(50).map(|n| context! {
        id => n.id,
        node_type => format!("{:?}", n.node_type),
        label => n.label,
        agent_id => n.agent_id,
        session_id => n.session_id,
    }).collect();

    let edges: Vec<_> = all_edges.iter().take(100).map(|e| context! {
        src_label => node_label.get(e.source_node_id.as_str()).copied().unwrap_or("?"),
        edge_type => format!("{:?}", e.edge_type),
        tgt_label => node_label.get(e.target_node_id.as_str()).copied().unwrap_or("?"),
    }).collect();

    let history: Vec<_> = event_history.iter().map(|u| context! {
        start_time => u.start_time,
        agent_role => u.agent_role,
        text => u.text,
    }).collect();

    let events: Vec<_> = new_events.iter().map(|ev| context! {
        id => ev.id,
        start_time => ev.start_time,
        agent_role => ev.agent_role,
        event_kind => format!("{:?}", ev.event_kind),
        tool_name => ev.tool_name.clone(),
        text => ev.text,
    }).collect();

    ENV.get_template("user_batch_extraction")
        .unwrap()
        .render(context! { nodes, edges, history, events, compressed_history })
        .unwrap()
}

pub fn build_extraction_tool_schema() -> Value {
    json!({
        "name": "extract_argument_nodes",
        "description": "Extract argument structure from the current event. Call with empty arrays if the event has no argumentative content.",
        "input_schema": {
            "type": "object",
            "properties": {
                "new_nodes": {
                    "type": "array",
                    "description": "New argument nodes found in this event",
                    "items": {
                        "type": "object",
                        "properties": {
                            "id":                  { "type": "string", "description": "A unique ID you assign to this node" },
                            "node_type":           { "type": "string", "enum": ["Decision", "Rule", "OpenItem"] },
                            "label":               { "type": "string", "description": "Short summary, max 15 words" },
                            "description":         { "type": "string", "description": "Longer explanation of this argument component" },
                            "event_id": { "type": "string", "description": "ID of the event this came from" },
                            "agent_id": { "type": "string", "description": "Agent ID who made the statement" },
                            "session_id": { "type": "string", "description": "Session identifier" },
                            "timestamp":           { "type": "number", "description": "Start time of the source event in seconds" },
                            "confidence":          { "type": "number", "description": "0.0 to 1.0" }
                        },
                        "required": ["id", "node_type", "label", "description", "event_id", "agent_id", "session_id", "timestamp", "confidence"]
                    }
                },
                "new_edges": {
                    "type": "array",
                    "description": "Edges connecting nodes. Use existing node IDs from the graph context for existing nodes.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "id":             { "type": "string", "description": "A unique ID for this edge" },
                            "edge_type":      { "type": "string", "enum": ["Supersedes", "Refutes", "Resolves", "RelatesTo"] },
                            "source_node_id": { "type": "string", "description": "ID of the source node" },
                            "target_node_id": { "type": "string", "description": "ID of the target node" },
                            "reasoning":      { "type": "string", "description": "One sentence explaining why this relation exists" },
                            "timestamp":      { "type": "number" },
                            "provenance":     { "type": "string", "enum": ["EXTRACTED", "INFERRED", "AMBIGUOUS"], "description": "How directly this relation was found: EXTRACTED if explicit in the text, INFERRED if deduced, AMBIGUOUS if uncertain" }
                        },
                        "required": ["id", "edge_type", "source_node_id", "target_node_id", "reasoning", "timestamp", "provenance"]
                    }
                }
            },
            "required": ["new_nodes", "new_edges"]
        }
    })
}

pub fn render_user_extraction(
    recent_nodes: &[ArgumentNode],
    all_edges: &[ArgumentEdge],
    event_history: &[armin_ingest::EventRecord],
    new_event: &armin_ingest::EventRecord,
    compressed_history: Option<&str>,
) -> String {
    let node_label: std::collections::HashMap<&str, &str> = recent_nodes
        .iter()
        .map(|n| (n.id.as_str(), n.label.as_str()))
        .collect();

    let nodes: Vec<_> = recent_nodes.iter().take(50).map(|n| context! {
        id => n.id,
        node_type => format!("{:?}", n.node_type),
        label => n.label,
        agent_id => n.agent_id,
        session_id => n.session_id,
    }).collect();

    let edges: Vec<_> = all_edges.iter().take(100).map(|e| context! {
        src_label => node_label.get(e.source_node_id.as_str()).copied().unwrap_or("?"),
        edge_type => format!("{:?}", e.edge_type),
        tgt_label => node_label.get(e.target_node_id.as_str()).copied().unwrap_or("?"),
    }).collect();

    let history: Vec<_> = event_history.iter().map(|u| context! {
        start_time => u.start_time,
        agent_role => u.agent_role,
        text => u.text,
    }).collect();

    let utterance = context! {
        start_time => new_event.start_time,
        agent_role => new_event.agent_role,
        text => new_event.text,
    };

    ENV.get_template("user_extraction")
        .unwrap()
        .render(context! { nodes, edges, history, utterance, compressed_history })
        .unwrap()
}

pub fn render_user_find_nodes(question: &str, node_labels: &[(String, String)]) -> String {
    let nodes: Vec<_> = node_labels.iter().map(|(id, label)| context! {
        id,
        label,
    }).collect();

    ENV.get_template("user_find_nodes")
        .unwrap()
        .render(context! { nodes, question })
        .unwrap()
}

pub fn render_user_answer_query(question: &str, subgraph: &armin_graph::GraphSnapshot) -> String {
    let nodes: Vec<_> = subgraph.nodes.iter().map(|n| context! {
        node_type => format!("{:?}", n.node_type),
        label => n.label,
        agent_id => n.agent_id,
        session_id => n.session_id,
        description => n.description,
    }).collect();

    let edges: Vec<_> = subgraph.edges.iter().map(|e| {
        let src = subgraph.nodes.iter().find(|n| n.id == e.source_node_id).map(|n| n.label.as_str()).unwrap_or("?");
        let tgt = subgraph.nodes.iter().find(|n| n.id == e.target_node_id).map(|n| n.label.as_str()).unwrap_or("?");
        context! {
            src_label => src,
            edge_type => format!("{:?}", e.edge_type),
            tgt_label => tgt,
            reasoning => e.reasoning,
        }
    }).collect();

    ENV.get_template("user_answer_query")
        .unwrap()
        .render(context! { nodes, edges, question })
        .unwrap()
}
