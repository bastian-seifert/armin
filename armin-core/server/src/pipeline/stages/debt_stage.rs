use async_trait::async_trait;

use crate::pipeline::{PipelineContext, Stage};
use crate::state::AppState;

/// Computes the reasoning debt report for the current session state.
pub struct DebtStage;

#[async_trait]
impl Stage for DebtStage {
    fn name(&self) -> &'static str {
        "debt"
    }

    async fn run(&self, state: &AppState, ctx: &mut PipelineContext) -> anyhow::Result<()> {
        ctx.debt = state.graph.compute_debt_report(ctx.session_idx).await;
        Ok(())
    }
}
