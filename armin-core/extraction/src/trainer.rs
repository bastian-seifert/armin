use std::path::Path;

use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tracing::error;

/// The kind of LLM operation being recorded.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationType {
    Extraction,
    FindNodes,
    AnswerQuery,
    Compression,
}

/// A single LLM interaction, persisted as one JSONL line for later distillation.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TrainingRecord {
    pub id: String,
    pub timestamp: String,
    pub provider: String,
    pub model: String,
    pub operation: OperationType,
    pub duration_ms: u64,
    /// Full request payload sent to the LLM provider (body only, no auth headers).
    pub request_body: serde_json::Value,
    /// Full response payload received from the LLM provider.
    pub response_body: serde_json::Value,
    /// Structured, parsed result from the provider response.
    pub parsed_result: serde_json::Value,
    /// Arbitrary extra metadata (event_id, question, etc.).
    pub extra: serde_json::Value,
}

/// Non-blocking JSONL recorder for LLM training data.
///
/// Records are sent over a bounded channel to a background writer task,
/// so the LLM call path never blocks on I/O. A periodic flush interval
/// ensures data is durable even if the process crashes.
const CHANNEL_CAPACITY: usize = 1024;
const FLUSH_INTERVAL_SECS: u64 = 5;

pub struct TrainingRecorder {
    tx: mpsc::Sender<TrainingRecord>,
}

impl TrainingRecorder {
    /// Open `path` in append-only mode and spawn a background writer task.
    pub fn new(path: impl AsRef<Path>) -> Self {
        let path = path.as_ref().to_path_buf();
        let (tx, mut rx) = mpsc::channel::<TrainingRecord>(CHANNEL_CAPACITY);

        tokio::spawn(async move {
            use tokio::io::AsyncWriteExt;

            let file = match tokio::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .await
            {
                Ok(f) => f,
                Err(e) => {
                    error!("Failed to open training data file {}: {e}", path.display());
                    return;
                }
            };
            let mut writer = tokio::io::BufWriter::new(file);
            let mut flush_ticker = tokio::time::interval(tokio::time::Duration::from_secs(FLUSH_INTERVAL_SECS));
            flush_ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

            loop {
                tokio::select! {
                    biased;
                    record = rx.recv() => {
                        let Some(record) = record else { break; };
                        let line = match serde_json::to_string(&record) {
                            Ok(s) => s,
                            Err(e) => {
                                error!("Failed to serialize training record: {e}");
                                continue;
                            }
                        };
                        if let Err(e) = writer.write_all(line.as_bytes()).await {
                            error!("Failed to write training record: {e}");
                            break;
                        }
                        if let Err(e) = writer.write_all(b"\n").await {
                            error!("Failed to write newline: {e}");
                            break;
                        }
                    }
                    _ = flush_ticker.tick() => {
                        if let Err(e) = writer.flush().await {
                            error!("Failed to flush training data: {e}");
                            break;
                        }
                    }
                }
            }

            let _ = writer.flush().await;
        });

        Self { tx }
    }

    /// Enqueue a record for writing. Returns immediately (non-blocking).
    pub fn record(&self, record: TrainingRecord) {
        if let Err(e) = self.tx.try_send(record) {
            error!("Training recorder channel full or closed — dropping record ({e})");
        }
    }
}
