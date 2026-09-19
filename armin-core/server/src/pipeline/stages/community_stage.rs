use anyhow::Result;
use async_trait::async_trait;

use crate::pipeline::{PipelineContext, Stage};
use crate::state::AppState;

/// Computes community detection on the argument graph.
///
/// Only recomputes every `RECOMPUTE_INTERVAL` events to avoid
/// unnecessary churn. The result is stored in PipelineContext so it
/// can be broadcast to WebSocket clients along with the current event.
pub struct CommunityStage {
    event_counter: std::sync::atomic::AtomicUsize,
}

const RECOMPUTE_INTERVAL: usize = 5;

impl CommunityStage {
    pub fn new() -> Self {
        Self {
            event_counter: std::sync::atomic::AtomicUsize::new(0),
        }
    }
}

#[async_trait]
impl Stage for CommunityStage {
    fn name(&self) -> &'static str {
        "community"
    }

    async fn run(&self, state: &AppState, ctx: &mut PipelineContext) -> Result<()> {
        let count = self
            .event_counter
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);

        if count.is_multiple_of(RECOMPUTE_INTERVAL) || ctx.graph_snapshot.nodes.len() < 10 {
            let report = state.graph.compute_community_report().await;
            ctx.community = Some(report);
        }

        Ok(())
    }
}
