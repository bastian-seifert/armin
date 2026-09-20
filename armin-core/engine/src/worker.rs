//! Background extraction worker.
//!
//! Prose events arrive over an unbounded channel and are batched: either
//! `batch_events` events accumulate or `batch_ms` elapses, whichever comes
//! first — then ONE extraction pass covers the whole batch (generative LLM
//! in `Llm` mode, TypeSafe System One judgments in `Jev` mode). The calling
//! agent's turn never waits on this loop; a slow or failing extractor
//! degrades graph freshness, not agent responsiveness.

use std::time::{Duration, Instant};

use armin_extraction::ExtractionClient;
use armin_graph::ExtractionResult;
use armin_ingest::EventRecord;
use tokio::sync::mpsc::UnboundedReceiver;
use tracing::{debug, info, warn};

use crate::state::EngineState;

/// Run the worker until the channel closes (i.e. the engine shuts down).
pub async fn run(state: EngineState, mut rx: UnboundedReceiver<EventRecord>) {
    info!(
        "Extraction worker started: mode={} batch_ms={} batch_events={} jev={}/{} llm_extractor={}",
        if state.jev.is_some() { "jev" } else { "llm" },
        state.config.batch_ms,
        state.config.batch_events,
        state.jev.as_ref().map(|j| j.provider()).unwrap_or("none"),
        state.jev.as_ref().map(|j| j.model().to_string()).unwrap_or_else(|| "none".into()),
        state.extractor.as_ref().map(|e| e.model_name()).unwrap_or_else(|| "none".into()),
    );

    loop {
        // Wait for the first event of a batch.
        let mut batch: Vec<EventRecord> = match rx.recv().await {
            Some(ev) => vec![ev],
            None => break,
        };

        // Accumulate until the batch is full or the debounce window closes.
        let deadline = tokio::time::Instant::now() + Duration::from_millis(state.config.batch_ms);
        loop {
            match tokio::time::timeout_at(deadline, rx.recv()).await {
                Ok(Some(ev)) => {
                    batch.push(ev);
                    if batch.len() >= state.config.batch_events {
                        break;
                    }
                }
                Ok(None) => {
                    process_batch(&state, &batch).await;
                    info!("Extraction channel closed; worker exiting");
                    return;
                }
                Err(_elapsed) => break,
            }
        }

        process_batch(&state, &batch).await;
    }
}

async fn process_batch(state: &EngineState, batch: &[EventRecord]) {
    if batch.is_empty() {
        return;
    }

    // Jev is the preferred path; the LLM extractor is the opt-out mode AND
    // the automatic fallback when no Typesafe key is configured.
    if state.jev.is_some() {
        process_batch_jev(state, batch).await;
    } else if state.extractor.is_some() {
        process_batch_llm(state, batch).await;
    } else {
        warn!(
            "No extractor available (jev needs TYPESAFE_AI_API_KEY, llm needs              ANTHROPIC/OPENAI_API_KEY) — dropping batch of {}",
            batch.len()
        );
    }
}

async fn process_batch_jev(state: &EngineState, batch: &[EventRecord]) {
    let Some(jev) = state.jev.clone() else {
        warn!("Jev mode but no Jev client configured; dropping batch of {}", batch.len());
        return;
    };

    // Earlier graph nodes serve as edge-pass context (cross-batch edges).
    let recent = state.graph.recent_snapshot(50, 100).await.nodes;

    let started = Instant::now();
    match armin_extraction::jev::extract_native_batch(
        &jev,
        batch,
        &recent,
        &state.config.jev_native,
    )
    .await
    {
        Ok(result) => {
            commit_result(state, result, batch, started, false).await;
        }
        Err(e) => {
            state
                .metrics
                .llm_extraction_errors
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            warn!(
                "Jev extraction failed for batch of {} events (events dropped from graph, \
                 tool evidence unaffected): {e}",
                batch.len()
            );
        }
    }
    let (calls, input_tokens, _) = jev.usage();
    state
        .metrics
        .jev_calls
        .store(calls, std::sync::atomic::Ordering::Relaxed);
    state
        .metrics
        .jev_input_tokens
        .store(input_tokens, std::sync::atomic::Ordering::Relaxed);
}

async fn process_batch_llm(state: &EngineState, batch: &[EventRecord]) {
    let Some(extractor) = state.extractor.clone() else {
        warn!(
            "LLM mode but no extractor configured; dropping batch of {}",
            batch.len()
        );
        return;
    };

    // Context: recent graph state (O(k), not a full clone) and conversation
    // history excluding the events being extracted themselves.
    let snapshot = state.graph.recent_snapshot(50, 100).await;
    let batch_ids: std::collections::HashSet<&str> = batch.iter().map(|e| e.id.as_str()).collect();
    let history: Vec<EventRecord> = state
        .recent_history()
        .await
        .into_iter()
        .filter(|e| !batch_ids.contains(e.id.as_str()))
        .collect();

    let started = Instant::now();
    let results = extractor
        .extract_batch(batch, &snapshot, &history, None)
        .await;

    match results {
        Ok(per_event) => {
            // Add ALL nodes across the batch before any edge: edges may
            // reference nodes bucketed under a different event.
            let mut result = ExtractionResult::default();
            for r in per_event {
                result.new_nodes.extend(r.new_nodes);
                result.new_edges.extend(r.new_edges);
            }
            commit_result(state, result, batch, started, true).await;
        }
        Err(e) => {
            state
                .metrics
                .llm_extraction_errors
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            warn!(
                "Extraction failed for batch of {} events (events dropped from graph, tool \
                 evidence unaffected): {e}",
                batch.len()
            );
        }
    }
}

/// Add an extraction result to the graph and update batch metrics.
/// `run_linker` enables the LLM resolution-linking second pass (Llm mode
/// only; the Jev edge pass emits Resolves edges itself).
async fn commit_result(
    state: &EngineState,
    result: ExtractionResult,
    batch: &[EventRecord],
    started: Instant,
    run_linker: bool,
) {
    let new_node_count = result.new_nodes.len();
    // Capture the fresh nodes for the linker before draining.
    let answer_tuples: Vec<(String, String, String)> = result
        .new_nodes
        .iter()
        .map(|n| (n.id.clone(), n.label.clone(), n.description.clone()))
        .collect();
    for node in result.new_nodes {
        state.graph.add_node(node).await;
    }
    let mut edges = 0usize;
    for edge in result.new_edges {
        if let Err(e) = state.graph.add_edge(edge).await {
            warn!("Worker failed to add edge: {e}");
        } else {
            edges += 1;
        }
    }
    let elapsed = started.elapsed().as_millis() as u64;
    state
        .metrics
        .llm_batches
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    state
        .metrics
        .llm_events_extracted
        .fetch_add(batch.len() as u64, std::sync::atomic::Ordering::Relaxed);
    state
        .metrics
        .nodes_added
        .fetch_add(new_node_count as u64, std::sync::atomic::Ordering::Relaxed);
    state
        .metrics
        .edges_added
        .fetch_add(edges as u64, std::sync::atomic::Ordering::Relaxed);
    state
        .metrics
        .last_batch_size
        .store(batch.len() as u64, std::sync::atomic::Ordering::Relaxed);
    state
        .metrics
        .last_extraction_ms
        .store(elapsed, std::sync::atomic::Ordering::Relaxed);
    debug!(
        "Extracted batch of {} events in {elapsed}ms: +{new_node_count} nodes +{edges} edges",
        batch.len()
    );

    // Second analytical pass: link fresh answers to open questions.
    if run_linker && state.config.linker && new_node_count > 0 {
        if let Some(extractor) = state.extractor.clone() {
            link_resolutions(state, &extractor, &answer_tuples).await;
        }
    }
}

/// Propose Resolves edges between still-open questions and the nodes this
/// batch just added. A second cheap LLM call per batch — negligible cost,
/// and it is what makes tracked questions actually resolve.
async fn link_resolutions(
    state: &EngineState,
    extractor: &ExtractionClient,
    answer_tuples: &[(String, String, String)],
) {
    let questions = state.graph.open_questions(25).await;
    if questions.is_empty() {
        debug!("Linker: no open questions; skipping");
        return;
    }
    let q_tuples: Vec<(String, String, String)> = questions
        .iter()
        .map(|n| (n.id.clone(), n.label.clone(), n.description.clone()))
        .collect();
    debug!(
        "Linker: {} open question(s), {} candidate answer node(s)",
        q_tuples.len(),
        answer_tuples.len()
    );

    match extractor.link_resolutions(&q_tuples, answer_tuples).await {
        Ok(links) if links.is_empty() => {
            debug!("Linker: no links proposed");
        }
        Ok(links) => {
            let mut added = 0u64;
            for (question_id, answer_id, reasoning) in links {
                if state
                    .graph
                    .resolve_question(&question_id, &answer_id, &reasoning)
                    .await
                    .is_ok()
                {
                    added += 1;
                }
            }
            if added > 0 {
                state
                    .metrics
                    .resolution_edges
                    .fetch_add(added, std::sync::atomic::Ordering::Relaxed);
                info!("Resolution linking: +{added} Resolves edge(s)");
            }
        }
        Err(e) => warn!("Resolution linking failed: {e}"),
    }
}
