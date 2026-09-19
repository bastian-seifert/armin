use async_trait::async_trait;
use armin_graph::ExtractionResult;
use tracing::warn;

use crate::pipeline::{PipelineContext, Stage};
use crate::state::AppState;

/// Calls the LLM extractor for the current event and stores the result.
pub struct ExtractionStage;

#[async_trait]
impl Stage for ExtractionStage {
    fn name(&self) -> &'static str {
        "extraction"
    }

    async fn run(&self, state: &AppState, ctx: &mut PipelineContext) -> anyhow::Result<()> {
        let result = state
            .extractor
            .extract(
                &armin_extraction::EventRecord {
                    id: ctx.event_id.clone(),
                    session_id: String::new(),
                    agent_role: ctx.event_agent_role.clone(),
                    start_time: ctx.event_start_time,
                    end_time: ctx.event_start_time,
                    text: ctx.event_text.clone(),
                    event_kind: Default::default(),
                    tool_name: None,
                    files: vec![],
                    commit: None,
                },
                &ctx.graph_snapshot,
                &ctx.event_history,
                ctx.compressed_history.as_deref(),
            )
            .await;

        match result {
            Ok(r) => ctx.extraction = r,
            Err(e) => {
                warn!("Extraction failed for {}: {e}", ctx.event_id);
                ctx.extraction = ExtractionResult::default();
            }
        }
        Ok(())
    }
}
