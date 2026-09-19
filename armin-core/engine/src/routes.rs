use std::sync::atomic::Ordering;

use axum::Json;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::response::Response;
use serde::Deserialize;
use serde_json::json;
use tracing::{info, warn};

use armin_ingest::EventKind;

use crate::answer::build_template_answer;
use crate::state::EngineState;

const MAX_DEPTH: usize = 10;

fn now_ts() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

// ── Health ───────────────────────────────────────────────────────────────────

pub async fn health(State(state): State<EngineState>) -> impl IntoResponse {
    Json(json!({
        "status": "ok",
        "extraction": if state.jev.is_some() {
            "jev"
        } else if state.extractor.is_some() {
            "llm"
        } else {
            "deterministic-only"
        },
        "db": state.db_path.as_ref().map(|p| p.display().to_string()),
        "sessions": state.sessions.read().await.len(),
    }))
}

// ── Ingest ───────────────────────────────────────────────────────────────────

/// Accept a batch of events. In v2, tool calls are not graph nodes (they
/// become scratch/provenance in WP2); prose events are queued for background
/// extraction. Always fast — never blocks on the LLM.
pub async fn ingest(
    State(state): State<EngineState>,
    Json(events): Json<Vec<armin_ingest::EventRecord>>,
) -> Response {
    let mut queued = 0usize;
    let captured = 0usize;
    for event in events {
        state.metrics.events_ingested.fetch_add(1, Ordering::Relaxed);
        state.register_session(&event.session_id).await;
        state.push_event_log(event.clone()).await;

        if event.event_kind == EventKind::ToolCall {
            state.metrics.tool_events.fetch_add(1, Ordering::Relaxed);
            let _ = crate::deterministic::is_tool_event(&event);
            crate::toolclass::record_tool_event(&state.scratch, &event).await;
        } else if let Some(tx) = &state.ingest_tx {
            if tx.send(event).is_ok() {
                queued += 1;
                state
                    .metrics
                    .events_queued_for_llm
                    .fetch_add(1, Ordering::Relaxed);
            }
        }
    }
    Json(json!({ "status": "ok", "queued_for_llm": queued, "captured_deterministically": captured }))
        .into_response()
}

/// Single-event ingest convenience endpoint.
pub async fn ingest_one(
    State(state): State<EngineState>,
    Json(event): Json<armin_ingest::EventRecord>,
) -> Response {
    ingest(
        State(state),
        Json(vec![event]),
    )
    .await
}

// ── Import (AGENTS.md / CLAUDE.md cold start) ────────────────────────────────

pub async fn import(
    State(state): State<EngineState>,
    Json(req): Json<crate::import::ImportRequest>,
) -> Response {
    let resp = crate::import::handle_import(&state.graph, req).await;
    state
        .metrics
        .nodes_added
        .fetch_add(resp.imported as u64, Ordering::Relaxed);
    Json(resp).into_response()
}

// ── Status page ───────────────────────────────────────────────────────────────

pub async fn ui() -> impl IntoResponse {
    axum::response::Html(include_str!("../ui/index.html"))
}

pub async fn root_redirect() -> impl IntoResponse {
    axum::response::Redirect::temporary("/ui")
}

// ── Reasoning-state brief ────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct BriefParams {
    /// Comma-separated file paths the agent is working on — scopes the
    /// "Binding here" section to rules/decisions covering these files.
    pub files: Option<String>,
}

pub async fn brief(
    State(state): State<EngineState>,
    Query(params): Query<BriefParams>,
) -> impl IntoResponse {
    let files: Vec<String> = params
        .files
        .map(|f| {
            f.split(',')
                .map(|p| p.trim().to_string())
                .filter(|p| !p.is_empty())
                .collect()
        })
        .unwrap_or_default();
    let brief = crate::brief::build_brief(&state, &files).await;
    Json(json!({ "brief": brief, "empty": brief.is_empty() }))
}

// ── Metrics ──────────────────────────────────────────────────────────────────

pub async fn metrics(State(state): State<EngineState>) -> impl IntoResponse {
    let mut snap = state.metrics.snapshot();
    if let Some(extractor) = &state.extractor {
        let (input, output) = extractor.token_usage();
        if let Some(obj) = snap.as_object_mut() {
            obj.insert("llm_input_tokens".into(), json!(input));
            obj.insert("llm_output_tokens".into(), json!(output));
        }
    }
    Json(snap)
}

// ── Runtime config ───────────────────────────────────────────────────────────

/// Update runtime settings (e.g. the harness pushes its live session model
/// after the first message). Only fields present in the body are changed.
pub async fn update_config(
    State(state): State<EngineState>,
    Json(update): Json<crate::state::ConfigUpdate>,
) -> Response {
    let mut applied_model: Option<String> = None;
    if let Some(model) = &update.model {
        match &state.extractor {
            Some(extractor) => {
                extractor.set_model(model);
                applied_model = Some(model.clone());
                info!("Extraction model updated at runtime: {model}");
            }
            None => {
                return (
                    StatusCode::CONFLICT,
                    "engine is running deterministic-only; model updates ignored",
                )
                    .into_response()
            }
        }
    }
    Json(json!({
        "status": "ok",
        "model": state.extractor.as_ref().map(|e| e.model_name()),
        "applied_model": applied_model,
    }))
    .into_response()
}

// ── Agent writes ─────────────────────────────────────────────────────────────

pub async fn agent_decision(
    State(state): State<EngineState>,
    Json(req): Json<armin_graph::AgentDecisionRequest>,
) -> Response {
    let node_id = uuid::Uuid::new_v4().to_string();
    let event_id = uuid::Uuid::new_v4().to_string();
    let agent_id = req.agent_id.clone().unwrap_or_else(|| "agent".to_string());

    let node = armin_graph::ArgumentNode {
        id: node_id.clone(),
        node_type: armin_graph::NodeType::Decision,
        label: req.label.clone(),
        description: req.description.clone(),
        event_id,
        agent_id,
        session_id: req.session_id.clone(),
        timestamp: now_ts(),
        confidence: 1.0,
        files: req.files.clone(),
        commit: req.commit.clone(),
        mention_count: 1,
        status: armin_graph::NodeStatus::Active,
    };
    state.graph.add_node(node).await;
    state.metrics.agent_writes.fetch_add(1, Ordering::Relaxed);
    state.metrics.nodes_added.fetch_add(1, Ordering::Relaxed);

    let mut edge_ids = Vec::new();
    for (i, target_id) in req.resolves.iter().enumerate() {
        let reasoning = req
            .resolutions
            .get(i)
            .cloned()
            .unwrap_or_else(|| format!("Decision '{}' resolves this", req.label));
        match state
            .graph
            .resolve_question(target_id, &node_id, &reasoning)
            .await
        {
            Ok(edge) => edge_ids.push(edge.id),
            Err(e) => warn!("Failed to resolve question {}: {e}", target_id),
        }
    }

    Json(json!({
        "node_id": node_id,
        "node_label": req.label,
        "edge_ids": edge_ids,
    }))
    .into_response()
}

pub async fn agent_question(
    State(state): State<EngineState>,
    Json(req): Json<armin_graph::AgentQuestionRequest>,
) -> Response {
    let node_id = uuid::Uuid::new_v4().to_string();
    let event_id = uuid::Uuid::new_v4().to_string();
    let agent_id = req.agent_id.clone().unwrap_or_else(|| "agent".to_string());

    let node = armin_graph::ArgumentNode {
        id: node_id.clone(),
        node_type: armin_graph::NodeType::OpenItem,
        label: req.label.clone(),
        description: req.description.clone(),
        event_id,
        agent_id,
        session_id: req.session_id.clone(),
        timestamp: now_ts(),
        confidence: 1.0,
        files: req.files.clone(),
        commit: req.commit.clone(),
        mention_count: 1,
        status: armin_graph::NodeStatus::Active,
    };
    state.graph.add_node(node).await;
    state.metrics.agent_writes.fetch_add(1, Ordering::Relaxed);
    state.metrics.nodes_added.fetch_add(1, Ordering::Relaxed);

    Json(json!({
        "node_id": node_id,
        "node_label": req.label,
        "edge_ids": [],
    }))
    .into_response()
}

// ── Graph queries ────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct GraphParams {
    pub at_time: Option<f64>,
}

#[derive(Deserialize)]
pub struct RecentParams {
    pub nodes: Option<usize>,
    pub edges: Option<usize>,
}

pub async fn snapshot(
    State(state): State<EngineState>,
    Query(params): Query<GraphParams>,
) -> impl IntoResponse {
    match params.at_time {
        Some(t) => Json(state.graph.snapshot_at_time(t).await),
        None => Json(state.graph.snapshot().await),
    }
}

pub async fn recent(
    State(state): State<EngineState>,
    Query(params): Query<RecentParams>,
) -> impl IntoResponse {
    let node_count = params.nodes.unwrap_or(50);
    let edge_count = params.edges.unwrap_or(100);
    let snap = state.graph.snapshot().await;
    let nodes: Vec<_> = snap.nodes.into_iter().rev().take(node_count).collect();
    let node_ids: std::collections::HashSet<String> = nodes.iter().map(|n| n.id.clone()).collect();
    let edges: Vec<_> = snap
        .edges
        .into_iter()
        .rev()
        .take(edge_count)
        .filter(|e| node_ids.contains(&e.source_node_id) || node_ids.contains(&e.target_node_id))
        .collect();
    Json(json!({ "nodes": nodes, "edges": edges }))
}

// ── Graph mutations ──────────────────────────────────────────────────────────

pub async fn add_node(
    State(state): State<EngineState>,
    Json(node): Json<armin_graph::ArgumentNode>,
) -> impl IntoResponse {
    state.graph.add_node(node).await;
    Json(json!({ "status": "ok" }))
}

pub async fn add_edge(
    State(state): State<EngineState>,
    Json(edge): Json<armin_graph::ArgumentEdge>,
) -> Response {
    match state.graph.add_edge(edge).await {
        Ok(_) => Json(json!({ "status": "ok" })).into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, format!("Failed to add edge: {e}")).into_response(),
    }
}

#[derive(Deserialize)]
pub struct ExtractionRequest {
    pub nodes: Vec<armin_graph::ArgumentNode>,
    pub edges: Vec<armin_graph::ArgumentEdge>,
}

pub async fn post_extraction(
    State(state): State<EngineState>,
    Json(req): Json<ExtractionRequest>,
) -> Response {
    let nodes_len = req.nodes.len();
    let edges_len = req.edges.len();
    for node in req.nodes {
        state.graph.add_node(node).await;
    }
    for edge in req.edges {
        if let Err(e) = state.graph.add_edge(edge).await {
            warn!("Failed to add extraction edge: {e}");
        }
    }
    state.metrics.nodes_added.fetch_add(nodes_len as u64, Ordering::Relaxed);
    state.metrics.edges_added.fetch_add(edges_len as u64, Ordering::Relaxed);
    Json(json!({ "nodes_added": nodes_len, "edges_added": edges_len })).into_response()
}

#[derive(Deserialize)]
pub struct InvalidateRequest {
    pub node_id: String,
    pub rationale: String,
}

pub async fn invalidate(
    State(state): State<EngineState>,
    Json(req): Json<InvalidateRequest>,
) -> Response {
    tracing::debug!(node = %req.node_id, rationale = %req.rationale, "invalidate");
    match state.graph.invalidate_node(&req.node_id).await {
        Ok(()) => {
            Json(json!({
                "node_id": req.node_id,
                "node_label": "invalidated",
                "edge_ids": [],
            }))
            .into_response()
        }
        Err(e) => (
            StatusCode::BAD_REQUEST,
            format!("Invalidation failed: {e}"),
        )
            .into_response(),
    }
}

#[derive(Deserialize)]
pub struct ResolveRequest {
    pub question_id: String,
    pub resolver_node_id: String,
    pub reasoning: String,
}

pub async fn resolve(
    State(state): State<EngineState>,
    Json(req): Json<ResolveRequest>,
) -> Response {
    match state
        .graph
        .resolve_question(&req.question_id, &req.resolver_node_id, &req.reasoning)
        .await
    {
        Ok(edge) => Json(json!({
            "node_id": req.question_id,
            "node_label": "question_resolved",
            "edge_ids": [edge.id],
        })).into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            format!("Resolution failed: {e}"),
        )
            .into_response(),
    }
}

// ── Search & traversal ─────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct FindNodesRequest {
    pub question: String,
    pub limit: Option<usize>,
}

pub async fn find_nodes(
    State(state): State<EngineState>,
    Json(req): Json<FindNodesRequest>,
) -> impl IntoResponse {
    let limit = req.limit.unwrap_or(5);
    let ids = state.retriever.find_relevant_nodes(&req.question, limit).await;
    Json(json!({ "node_ids": ids }))
}

#[derive(Deserialize)]
pub struct BfsRequest {
    pub seed_ids: Vec<String>,
    pub depth: Option<usize>,
}

pub async fn bfs(
    State(state): State<EngineState>,
    Json(req): Json<BfsRequest>,
) -> impl IntoResponse {
    let depth = req.depth.unwrap_or(3).min(MAX_DEPTH);
    let subgraph = state.graph.bfs_subgraph(&req.seed_ids, depth).await;
    Json(subgraph)
}

// ── Deterministic query ────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct QueryRequest {
    pub question: String,
    pub depth: Option<usize>,
}

pub async fn query(
    State(state): State<EngineState>,
    Json(req): Json<QueryRequest>,
) -> impl IntoResponse {
    let depth = req.depth.unwrap_or(3).min(MAX_DEPTH);
    let seed_ids = state.retriever.find_relevant_nodes(&req.question, 5).await;
    let subgraph = state.graph.bfs_subgraph(&seed_ids, depth).await;
    let mut result = state.graph.query_subgraph(&req.question, &subgraph).await;
    result.answer = build_template_answer(&result.trace, &subgraph);
    Json(result)
}

// ── Algorithm results ──────────────────────────────────────────────────────────

pub async fn debt(State(state): State<EngineState>) -> impl IntoResponse {
    let session_idx = state.current_session_idx.load(Ordering::SeqCst);
    let scratch = state.scratch.snapshot(&state.current_session_id().await).await;
    let report = state.graph.compute_debt_report_with(session_idx, Some(&scratch)).await;
    // Remember this report so the next /summary can express a delta.
    *state.prior_debt.write().await = Some(report.clone());
    Json(report)
}

pub async fn decisions(State(state): State<EngineState>) -> impl IntoResponse {
    let session_idx = state.current_session_idx.load(Ordering::SeqCst);
    Json(state.graph.extract_decisions(session_idx).await)
}

pub async fn risks(State(state): State<EngineState>) -> impl IntoResponse {
    let session_idx = state.current_session_idx.load(Ordering::SeqCst);
    Json(state.graph.compute_risks(session_idx).await)
}

pub async fn summary(State(state): State<EngineState>) -> impl IntoResponse {
    let session_idx = state.current_session_idx.load(Ordering::SeqCst);
    let prior_debt = state.prior_debt.read().await.clone();
    Json(state.graph.compute_summary(session_idx, prior_debt.as_ref()).await)
}

pub async fn communities(State(state): State<EngineState>) -> impl IntoResponse {
    Json(state.graph.compute_community_report().await)
}

// ── Session diff ───────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct DiffQuery {
    pub from: f64,
    pub to: f64,
}

pub async fn diff(
    State(state): State<EngineState>,
    Query(params): Query<DiffQuery>,
) -> impl IntoResponse {
    Json(state.graph.compute_diff(params.from, params.to).await)
}
