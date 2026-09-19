pub mod stages;

use async_trait::async_trait;
use armin_graph::{CommunityReport, DebtReport, ExtractionResult, GraphSnapshot};

use crate::state::AppState;

/// Shared context passed through every pipeline stage.
pub struct PipelineContext {
    pub event_id: String,
    pub event_text: String,
    pub event_start_time: f64,
    pub event_agent_role: String,
    pub event_history: Vec<armin_extraction::EventRecord>,
    pub graph_snapshot: GraphSnapshot,
    pub extraction: ExtractionResult,
    pub debt: DebtReport,
    pub community: Option<CommunityReport>,
    pub session_idx: usize,
    pub compressed_history: Option<String>,
}

#[async_trait]
pub trait Stage: Send + Sync {
    #[allow(dead_code)]
    fn name(&self) -> &'static str;
    async fn run(&self, state: &AppState, ctx: &mut PipelineContext) -> anyhow::Result<()>;
}

/// A sequence of stages that run in order over a single event.
pub struct Pipeline {
    stages: Vec<Box<dyn Stage>>,
}

impl Pipeline {
    pub fn new() -> Self {
        Self { stages: Vec::new() }
    }

    pub fn with_stage(mut self, stage: Box<dyn Stage>) -> Self {
        self.stages.push(stage);
        self
    }

    pub async fn process(&self, state: &AppState, ctx: &mut PipelineContext) -> anyhow::Result<()> {
        for stage in &self.stages {
            stage.run(state, ctx).await?;
        }
        Ok(())
    }
}
