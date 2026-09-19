use std::sync::atomic::Ordering;

use axum::Json;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use armin_graph::{
    AgentDecisionRequest, AgentInvalidateRequest, AgentQuestionRequest, AgentResolveRequest,
    AgentWriteResponse, CommunityReport, DebtReport, Decision, ExecutiveSummary, GraphDiff,
    GraphSnapshot, NodeStatus, NodeType, QueryResult, Risk,
};
use serde::Deserialize;
use serde_json::json;
use tracing::warn;

use crate::answer::build_template_answer;
use crate::pipeline::PipelineContext;
use crate::state::AppState;

const MAX_DEPTH: usize = 10;

#[derive(Deserialize)]
pub struct QueryRequest {
    pub question: String,
    pub mode: Option<String>,
    pub depth: Option<usize>,
}

#[derive(Deserialize)]
pub struct GraphParams {
    pub at_time: Option<f64>,
}

#[derive(Deserialize)]
pub struct DiffQuery {
    pub from: f64,
    pub to: f64,
}

fn now_ts() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

fn broadcast_event(state: &AppState, new_nodes: &[armin_graph::ArgumentNode], new_edges: &[armin_graph::ArgumentEdge]) {
    let msg = json!({
        "type": "event",
        "data": null,
        "new_nodes": new_nodes,
        "new_edges": new_edges,
        "debt_report": null,
        "community_report": null,
    })
    .to_string();
    if let Err(e) = state.ws_tx.send(msg) {
        tracing::debug!("No WS receivers for agent write broadcast: {e}");
    }
}

pub async fn agent_decision_handler(
    State(state): State<AppState>,
    Json(req): Json<AgentDecisionRequest>,
) -> impl IntoResponse {
    let node_id = uuid::Uuid::new_v4().to_string();
    let event_id = uuid::Uuid::new_v4().to_string();
    let ts = now_ts();
    let agent_id = req.agent_id.unwrap_or_else(|| "agent".to_string());

    let node = armin_graph::ArgumentNode {
        id: node_id.clone(),
        node_type: NodeType::Decision,
        label: req.label,
        description: req.description,
        event_id,
        agent_id,
        session_id: req.session_id,
        timestamp: ts,
        confidence: 1.0,
        files: req.files,
        commit: req.commit,
        mention_count: 1,
        status: NodeStatus::Active,
    };

    state.graph.add_node(node.clone()).await;

    let mut edge_ids = Vec::new();
    let mut edges = Vec::new();
    for (i, target_id) in req.resolves.iter().enumerate() {
        let reasoning = req.resolutions.get(i).cloned().unwrap_or_else(|| {
            format!("Decision '{}' resolves this", node.label)
        });
        match state.graph.resolve_question(target_id, &node_id, &reasoning).await {
            Ok(edge) => {
                edge_ids.push(edge.id.clone());
                edges.push(edge);
            }
            Err(e) => {
                tracing::warn!("Failed to resolve question {}: {e}", target_id);
            }
        }
    }

    broadcast_event(&state, &[node.clone()], &edges);

    Json(AgentWriteResponse {
        node_id,
        node_label: node.label,
        edge_ids,
    })
}

pub async fn agent_question_handler(
    State(state): State<AppState>,
    Json(req): Json<AgentQuestionRequest>,
) -> impl IntoResponse {
    let node_id = uuid::Uuid::new_v4().to_string();
    let event_id = uuid::Uuid::new_v4().to_string();
    let ts = now_ts();
    let agent_id = req.agent_id.unwrap_or_else(|| "agent".to_string());

    let node = armin_graph::ArgumentNode {
        id: node_id.clone(),
        node_type: NodeType::OpenItem,
        label: req.label,
        description: req.description,
        event_id,
        agent_id,
        session_id: req.session_id,
        timestamp: ts,
        confidence: 1.0,
        files: req.files,
        commit: req.commit,
        mention_count: 1,
        status: NodeStatus::Active,
    };

    state.graph.add_node(node.clone()).await;
    broadcast_event(&state, &[node.clone()], &[]);

    Json(AgentWriteResponse {
        node_id,
        node_label: node.label,
        edge_ids: vec![],
    })
}

pub async fn agent_resolve_handler(
    State(state): State<AppState>,
    Json(req): Json<AgentResolveRequest>,
) -> impl IntoResponse {
    match state.graph.resolve_question(&req.question_id, &req.resolver_node_id, &req.reasoning).await {
        Ok(edge) => {
            broadcast_event(&state, &[], &[edge.clone()]);
            Json(AgentWriteResponse {
                node_id: req.question_id.clone(),
                node_label: "question_resolved".to_string(),
                edge_ids: vec![edge.id],
            }).into_response()
        }
        Err(e) => (
            StatusCode::BAD_REQUEST,
            format!("Resolution failed: {e}"),
        ).into_response(),
    }
}

pub async fn agent_invalidate_handler(
    State(state): State<AppState>,
    Json(req): Json<AgentInvalidateRequest>,
) -> impl IntoResponse {
    match state.graph.invalidate_node(&req.node_id).await {
        Ok(()) => {
            // Also create a short Fact node capturing the rationale, with a Refutes edge
            let claim_id = uuid::Uuid::new_v4().to_string();
            let claim = armin_graph::ArgumentNode {
                id: claim_id.clone(),
                node_type: NodeType::Decision,
                label: format!("Invalidated: {}", req.rationale.chars().take(60).collect::<String>()),
                description: req.rationale,
                event_id: uuid::Uuid::new_v4().to_string(),
                agent_id: "agent".to_string(),
                session_id: String::new(),
                timestamp: now_ts(),
                confidence: 1.0,
                files: vec![],
                commit: None,
                mention_count: 1,
                status: NodeStatus::Active,
            };
            state.graph.add_node(claim.clone()).await;

            let edge = armin_graph::ArgumentEdge {
                id: uuid::Uuid::new_v4().to_string(),
                edge_type: armin_graph::EdgeType::Refutes,
                source_node_id: claim_id.clone(),
                target_node_id: req.node_id.clone(),
                reasoning: format!("Invalidated: {}", claim.description),
                timestamp: now_ts(),
                evidence_score: None,
                provenance: armin_graph::EdgeProvenance::Extracted,
            };
            let _ = state.graph.add_edge(edge.clone()).await;

            broadcast_event(&state, &[claim.clone()], &[edge.clone()]);

            Json(AgentWriteResponse {
                node_id: req.node_id,
                node_label: "invalidated".to_string(),
                edge_ids: vec![edge.id],
            }).into_response()
        }
        Err(e) => (
            StatusCode::BAD_REQUEST,
            format!("Invalidation failed: {e}"),
        ).into_response(),
    }
}

pub async fn query_handler(
    State(state): State<AppState>,
    Json(req): Json<QueryRequest>,
) -> impl IntoResponse {
    let mode = req.mode.as_deref().unwrap_or("deterministic");
    let depth = req.depth.map(|d| d.min(MAX_DEPTH));

    match mode {
        "llm" => handle_llm_query(&state, &req.question, depth).await.into_response(),
        "deterministic" | "embedding" | "hybrid" => {
            handle_deterministic_query(&state, &req.question, depth, mode).await.into_response()
        }
        other => (
            StatusCode::BAD_REQUEST,
            format!("Unknown query mode '{other}'. Use 'deterministic', 'llm', 'embedding', or 'hybrid'."),
        )
            .into_response(),
    }
}

/// Accept events pushed from external sources (no `--stream` file needed).
/// Runs the full pipeline per event and broadcasts results to WS clients.
pub async fn ingest_handler(
    State(state): State<AppState>,
    Json(events): Json<Vec<armin_ingest::EventRecord>>,
) -> impl IntoResponse {
    for record in events {
        // Update event log
        {
            let mut log = state.event_log.write().await;
            if log.len() >= 20 {
                log.pop_front();
            }
            log.push_back(record.clone());
        }

        let history: Vec<armin_ingest::EventRecord> = {
            let log = state.event_log.read().await;
            log.iter().cloned().collect()
        };
        let snapshot = state.graph.snapshot().await;
        let session_idx = state.current_session_idx.load(Ordering::SeqCst);

        let mut ctx = PipelineContext {
            event_id: record.id.clone(),
            event_text: record.text.clone(),
            event_start_time: record.start_time,
            event_agent_role: record.agent_role.clone(),
            event_history: history,
            graph_snapshot: snapshot,
            extraction: armin_graph::ExtractionResult::default(),
            debt: armin_graph::DebtReport {
                items: vec![],
                total_score: 0,
                timestamp: 0.0,
            },
            community: None,
            session_idx,
            compressed_history: None,
        };

        if let Err(e) = state.pipeline.process(&state, &mut ctx).await {
            warn!("Pipeline error for {}: {e}", record.id);
        }

        for node in &ctx.extraction.new_nodes {
            state.graph.add_node(node.clone()).await;
        }
        for edge in &ctx.extraction.new_edges {
            if let Err(e) = state.graph.add_edge(edge.clone()).await {
                warn!("Failed to add edge {}: {e}", edge.id);
            }
        }

        broadcast_event(&state, &ctx.extraction.new_nodes, &ctx.extraction.new_edges);
    }

    Json(json!({ "status": "ok" }))
}

async fn handle_deterministic_query(state: &AppState, question: &str, depth: Option<usize>, _mode: &str) -> Json<QueryResult> {
    let depth = depth.unwrap_or(3).min(MAX_DEPTH);

    // Step 1: find relevant nodes via retriever trait
    let seed_ids = state.retriever.find_relevant_nodes(question, 5).await;

    // Step 2: BFS subgraph (configurable depth, default 3)
    let subgraph = state.graph.bfs_subgraph(&seed_ids, depth).await;

    // Step 3: deterministic answer generation
    let mut result = state.graph.query_subgraph(question, &subgraph).await;
    result.answer = build_template_answer(&result.trace, &subgraph);
    Json(result)
}

async fn handle_llm_query(state: &AppState, question: &str, depth: Option<usize>) -> Json<QueryResult> {
    let depth = depth.unwrap_or(3).min(MAX_DEPTH);

    // Step 1: find relevant nodes via LLM
    let node_labels = state.graph.all_node_labels().await;
    let seed_ids = match state
        .extractor
        .find_relevant_nodes(question, &node_labels)
        .await
    {
        Ok(ids) => ids,
        Err(e) => {
            warn!("find_relevant_nodes failed: {e}");
            vec![]
        }
    };

    // Fall back to BM25 if LLM found nothing
    let seed_ids = if seed_ids.is_empty() {
        warn!("LLM found no relevant nodes, falling back to BM25");
        state.retriever.find_relevant_nodes(question, 5).await
    } else {
        seed_ids
    };

    // Step 2: BFS subgraph (configurable depth, default 3)
    let subgraph = state.graph.bfs_subgraph(&seed_ids, depth).await;

    // Step 3: LLM answer generation
    match state.extractor.answer_query(question, &subgraph).await {
        Ok(result) => Json(result),
        Err(e) => {
            warn!("answer_query failed: {e}");
            Json(QueryResult {
                answer: format!("Query failed: {e}"),
                trace: vec![],
                cited_events: vec![],
                mode_used: "llm".to_string(),
            })
        }
    }
}

pub async fn graph_handler(
    State(state): State<AppState>,
    Query(params): Query<GraphParams>,
) -> Json<GraphSnapshot> {
    match params.at_time {
        Some(t) => Json(state.graph.snapshot_at_time(t).await),
        None => Json(state.graph.snapshot().await),
    }
}

pub async fn debt_handler(State(state): State<AppState>) -> Json<DebtReport> {
    let session_idx = state.current_session_idx.load(Ordering::SeqCst);
    Json(state.graph.compute_debt_report(session_idx).await)
}

pub async fn community_handler(State(state): State<AppState>) -> Json<CommunityReport> {
    Json(state.graph.compute_community_report().await)
}

pub async fn decisions_handler(State(state): State<AppState>) -> Json<Vec<Decision>> {
    let session_idx = state.current_session_idx.load(Ordering::SeqCst);
    Json(state.graph.extract_decisions(session_idx).await)
}

pub async fn risks_handler(State(state): State<AppState>) -> Json<Vec<Risk>> {
    let session_idx = state.current_session_idx.load(Ordering::SeqCst);
    Json(state.graph.compute_risks(session_idx).await)
}

pub async fn summary_handler(State(state): State<AppState>) -> Json<ExecutiveSummary> {
    let session_idx = state.current_session_idx.load(Ordering::SeqCst);
    let prior_debt = state.prior_debt_report.read().await.clone();
    Json(state.graph.compute_summary(session_idx, prior_debt.as_ref()).await)
}

pub async fn diff_handler(
    State(state): State<AppState>,
    Query(params): Query<DiffQuery>,
) -> Json<GraphDiff> {
    Json(state.graph.compute_diff(params.from, params.to).await)
}
