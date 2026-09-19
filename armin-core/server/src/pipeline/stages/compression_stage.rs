use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use tracing::info;

use crate::pipeline::{PipelineContext, Stage};
use crate::state::AppState;

/// Configuration for the compression stage.
#[derive(Clone, Debug)]
pub struct CompressionConfig {
    /// How many events to batch before triggering compression (0 = disabled, default 5).
    pub interval: usize,
    /// Whether compression is enabled.
    pub enabled: bool,
}

impl Default for CompressionConfig {
    fn default() -> Self {
        Self { interval: 5, enabled: false }
    }
}

pub struct CompressionStage {
    config: CompressionConfig,
    /// Counter of events processed in the current batch.
    batch_counter: AtomicUsize,
}

impl CompressionStage {
    pub fn new(config: CompressionConfig) -> Self {
        Self {
            config,
            batch_counter: AtomicUsize::new(0),
        }
    }
}

#[async_trait]
impl Stage for CompressionStage {
    fn name(&self) -> &'static str {
        "compression"
    }

    async fn run(&self, state: &AppState, ctx: &mut PipelineContext) -> anyhow::Result<()> {
        if !self.config.enabled || self.config.interval == 0 {
            return Ok(());
        }

        let counter = self.batch_counter.fetch_add(1, Ordering::Relaxed) + 1;

        // Load the current running summary from AppState
        let current_summary = {
            let guard = state.compressed_summary.read().await;
            guard.clone()
        };

        // Every N events, compress the oldest events into the summary
        if counter.is_multiple_of(self.config.interval) && ctx.event_history.len() >= self.config.interval {
            let events_to_compress: Vec<&armin_extraction::EventRecord> = ctx.event_history
                .iter()
                .take(self.config.interval)
                .collect();

            let text_to_compress: String = events_to_compress
                .iter()
                .map(|e| format!("[{}] {}: {}", e.start_time, e.agent_role, e.text))
                .collect::<Vec<_>>()
                .join("\n");

            // Build compression prompt
            let system = "You are a conversation summarizer. Produce a concise 3-5 sentence summary of the key decisions, claims, and unresolved questions from these conversation events. Focus on preserving argument structure.";
            let user_msg = format!(
                "{}\n\n---\nPrevious summary:\n{}\n\n---\nNew events to incorporate:\n{}",
                system,
                current_summary.as_deref().unwrap_or("(none yet)"),
                text_to_compress
            );

            let summary = state.extractor.compress_text(system, &user_msg).await?;

            // Update the running summary
            {
                let mut guard = state.compressed_summary.write().await;
                *guard = Some(if current_summary.is_some() {
                    format!("{}\n\nAdditional context:\n{}", current_summary.as_deref().unwrap_or(""), summary)
                } else {
                    summary
                });
            }

            info!(
                "Compression: compressed {} events, summary now {} chars",
                events_to_compress.len(),
                state.compressed_summary.read().await.as_deref().unwrap_or("").len()
            );
        }

        // Set compressed_history on the context for downstream stages
        ctx.compressed_history = {
            let guard = state.compressed_summary.read().await;
            guard.clone()
        };

        Ok(())
    }
}

impl std::fmt::Debug for CompressionStage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CompressionStage")
            .field("config", &self.config)
            .finish()
    }
}
