use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::json;

/// Runtime counters for the extraction pipeline. Exposed via `GET /metrics`
/// so the embedding harness (and operators) can verify cost/latency behavior.
#[derive(Default)]
pub struct Metrics {
    pub started_at: AtomicU64,
    pub events_ingested: AtomicU64,
    pub tool_events: AtomicU64,
    pub events_queued_for_llm: AtomicU64,
    pub llm_batches: AtomicU64,
    pub llm_events_extracted: AtomicU64,
    pub llm_extraction_errors: AtomicU64,
    pub deterministic_nodes: AtomicU64,
    pub nodes_added: AtomicU64,
    pub edges_added: AtomicU64,
    pub agent_writes: AtomicU64,
    pub queries: AtomicU64,
    pub resolution_edges: AtomicU64,
    pub last_batch_size: AtomicU64,
    pub last_extraction_ms: AtomicU64,
    pub jev_calls: AtomicU64,
    pub jev_input_tokens: AtomicU64,
}

macro_rules! counter_json {
    ($self:expr, $(($field:ident, $name:literal)),+ $(,)?) => {
        json!({
            $($name: $self.$field.load(Ordering::Relaxed),)+
        })
    };
}

impl Metrics {
    pub fn snapshot(&self) -> serde_json::Value {
        let mut base = counter_json!(
            self,
            (started_at, "started_at"),
            (events_ingested, "events_ingested"),
            (tool_events, "tool_events"),
            (events_queued_for_llm, "events_queued_for_llm"),
            (llm_batches, "llm_batches"),
            (llm_events_extracted, "llm_events_extracted"),
            (llm_extraction_errors, "llm_extraction_errors"),
            (deterministic_nodes, "deterministic_nodes"),
            (nodes_added, "nodes_added"),
            (edges_added, "edges_added"),
            (agent_writes, "agent_writes"),
            (queries, "queries"),
            (resolution_edges, "resolution_edges"),
        );
        if let Some(obj) = base.as_object_mut() {
            obj.insert(
                "last_batch_size".into(),
                json!(self.last_batch_size.load(Ordering::Relaxed)),
            );
            obj.insert(
                "last_extraction_ms".into(),
                json!(self.last_extraction_ms.load(Ordering::Relaxed)),
            );
            obj.insert(
                "jev_calls".into(),
                json!(self.jev_calls.load(Ordering::Relaxed)),
            );
            obj.insert(
                "jev_input_tokens".into(),
                json!(self.jev_input_tokens.load(Ordering::Relaxed)),
            );
        }
        base
    }
}
