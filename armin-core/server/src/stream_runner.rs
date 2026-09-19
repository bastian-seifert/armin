use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::time::Duration;

use anyhow::Result;
use armin_ingest::{EventRecord, EventSource, JsonlSource};
use serde_json::json;
use tracing::{info, warn};

use crate::pipeline::PipelineContext;
use crate::state::AppState;

pub async fn run_stream(state: AppState, path: PathBuf, speed: u32) -> Result<()> {
    let mut source = JsonlSource::from_path(path).await?;
    run_with_source(state, &mut source, speed).await
}

async fn run_with_source(state: AppState, source: &mut dyn EventSource, speed: u32) -> Result<()> {
    let mut prev_time: Option<f64> = None;

    while let Some(record) = source.next_event().await? {
        apply_delay(prev_time, record.start_time, speed).await;
        prev_time = Some(record.start_time);

        // Update event log
        {
            let mut log = state.event_log.write().await;
            if log.len() >= 20 {
                log.pop_front();
            }
            log.push_back(record.clone());
        }

        // Build pipeline context
        let history: Vec<EventRecord> = {
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

        // Run pipeline stages
        if let Err(e) = state.pipeline.process(&state, &mut ctx).await {
            warn!("Pipeline error for {}: {e}", record.id);
        }

        // Update graph with extraction results
        for node in &ctx.extraction.new_nodes {
            state.graph.add_node(node.clone()).await;
        }
        for edge in &ctx.extraction.new_edges {
            if let Err(e) = state.graph.add_edge(edge.clone()).await {
                warn!("Failed to add edge {}: {e}", edge.id);
            }
        }

        let msg = json!({
            "type": "event",
            "data": record,
            "new_nodes": ctx.extraction.new_nodes,
            "new_edges": ctx.extraction.new_edges,
            "debt_report": ctx.debt,
            "community_report": ctx.community,
        })
        .to_string();
        if let Err(e) = state.ws_tx.send(msg) {
            tracing::debug!("No WS receivers for event broadcast: {e}");
        }
    }

    info!("Stream complete");
    Ok(())
}

async fn apply_delay(prev_time: Option<f64>, current_time: f64, speed: u32) {
    if speed == 0 {
        return;
    }
    if let Some(prev) = prev_time {
        let gap = (current_time - prev).max(0.0);
        let sleep_ms = (gap * 1000.0 / speed as f64) as u64;
        if sleep_ms > 0 {
            tokio::time::sleep(Duration::from_millis(sleep_ms)).await;
        }
    }
}
