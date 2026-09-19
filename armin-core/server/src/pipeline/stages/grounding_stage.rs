use std::sync::Arc;

use async_trait::async_trait;

use crate::pipeline::{PipelineContext, Stage};
use crate::state::AppState;

/// Searches for external evidence supporting each new edge and assigns an
/// evidence_score.  Skips silently when no searcher is configured.
pub struct GroundingStage {
    pub searcher: Option<Arc<dyn armin_grounding::EvidenceSearcher>>,
}

impl GroundingStage {
    /// Build a query from an edge's reasoning and the labels of its endpoints.
    fn build_query(&self, edge: &armin_graph::ArgumentEdge, ctx: &PipelineContext) -> String {
        let src_label = ctx
            .extraction
            .new_nodes
            .iter()
            .chain(
                ctx.graph_snapshot
                    .nodes
                    .iter(),
            )
            .find(|n| n.id == edge.source_node_id)
            .map(|n| n.label.as_str())
            .unwrap_or("unknown");

        let tgt_label = ctx
            .extraction
            .new_nodes
            .iter()
            .chain(
                ctx.graph_snapshot
                    .nodes
                    .iter(),
            )
            .find(|n| n.id == edge.target_node_id)
            .map(|n| n.label.as_str())
            .unwrap_or("unknown");

        format!(
            "{} {} {} evidence",
            src_label,
            edge.reasoning,
            tgt_label,
        )
    }
}

#[async_trait]
impl Stage for GroundingStage {
    fn name(&self) -> &'static str {
        "grounding"
    }

    async fn run(&self, _state: &AppState, ctx: &mut PipelineContext) -> anyhow::Result<()> {
        let Some(searcher) = &self.searcher else {
            return Ok(());
        };

        // Build queries first (immutable borrow), then mutate edges by index.
        let queries: Vec<String> = ctx
            .extraction
            .new_edges
            .iter()
            .map(|edge| self.build_query(edge, ctx))
            .collect();

        for (i, query) in queries.iter().enumerate() {
            let results = searcher.search(query).await?;
            let score = armin_grounding::EvidenceScorer::aggregate(query, &results);
            ctx.extraction.new_edges[i].evidence_score = Some(score);
        }

        Ok(())
    }
}
