use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Result, anyhow};
use armin_graph::{EdgeProvenance, EdgeType, ExtractionResult, GraphSnapshot, QueryResult};
use armin_ingest::EventRecord;
use serde_json::json;
use tracing::{debug, warn};

use crate::prompt::{
    build_extraction_tool_schema, render_system_batch_extraction, render_user_batch_extraction,
    render_user_find_nodes, render_user_answer_query,
};
use crate::provider::{AnthropicProvider, LlmProvider, OpenAiProvider, budget_hint, truncate_for_log};
use crate::trainer::{OperationType, TrainingRecord, TrainingRecorder};

const MIN_WORDS_FOR_EXTRACTION: usize = 5;

fn nonempty(var: Result<String, std::env::VarError>) -> Result<String> {
    var.ok()
        .filter(|k| !k.trim().is_empty())
        .ok_or_else(|| anyhow!("API key is unset or empty"))
}

fn api_key_for(provider_name: &str) -> Result<String> {
    match provider_name {
        "openai" => nonempty(std::env::var("OPENAI_API_KEY"))
            .map_err(|_| anyhow!("LLM_PROVIDER=openai but OPENAI_API_KEY is not set")),
        _ => nonempty(std::env::var("ANTHROPIC_API_KEY"))
            .map_err(|_| anyhow!("LLM_PROVIDER=anthropic but ANTHROPIC_API_KEY is not set")),
    }
}

#[derive(Clone)]
pub struct ExtractionClient {
    provider: Arc<dyn LlmProvider>,
    /// API key for the *selected* provider. Stored at construction so the
    /// provider and the key can never disagree (e.g. OpenAI provider paired
    /// with an Anthropic key).
    api_key: String,
    http: reqwest::Client,
    recorder: Option<Arc<TrainingRecorder>>,
    /// Self-healing flag: when a gateway ignores forced tool use (observed
    /// once), subsequent extraction calls switch to JSON-in-content mode.
    json_mode: Arc<std::sync::atomic::AtomicBool>,
    /// Cumulative token usage across all calls (for /metrics).
    input_tokens: Arc<std::sync::atomic::AtomicU64>,
    output_tokens: Arc<std::sync::atomic::AtomicU64>,
}

/// Extract (input, output) token counts from a provider response, handling
/// both OpenAI (`prompt_tokens`/`completion_tokens`) and Anthropic
/// (`input_tokens`/`output_tokens`) shapes.
fn meter_usage(
    input: &std::sync::atomic::AtomicU64,
    output: &std::sync::atomic::AtomicU64,
    raw: &serde_json::Value,
) {
    use std::sync::atomic::Ordering;
    let usage = raw.get("usage");
    if let Some(usage) = usage {
        let it = usage["prompt_tokens"]
            .as_u64()
            .or_else(|| usage["input_tokens"].as_u64())
            .unwrap_or(0);
        let ot = usage["completion_tokens"]
            .as_u64()
            .or_else(|| usage["output_tokens"].as_u64())
            .unwrap_or(0);
        input.fetch_add(it, Ordering::Relaxed);
        output.fetch_add(ot, Ordering::Relaxed);
    }
}

impl ExtractionClient {
    /// Build a client for the given provider using the matching API key.
    ///
    /// `LLM_PROVIDER` selects the provider explicitly ("anthropic" |
    /// "openai"); otherwise the provider is inferred from whichever API key
    /// is present, defaulting to anthropic.
    pub fn resolve() -> Result<Self> {
        let provider_name = std::env::var("LLM_PROVIDER").unwrap_or_default();
        let anthropic_key = std::env::var("ANTHROPIC_API_KEY").ok().filter(|k| !k.trim().is_empty());
        let openai_key = std::env::var("OPENAI_API_KEY").ok().filter(|k| !k.trim().is_empty());

        let (provider_name, api_key) = match provider_name.as_str() {
            "openai" => ("openai", api_key_for("openai")?),
            "anthropic" => ("anthropic", api_key_for("anthropic")?),
            _ => {
                // Infer: openai only if it's the sole available key.
                match (anthropic_key, openai_key) {
                    (Some(key), _) => Ok(("anthropic", key)),
                    (None, Some(key)) => Ok(("openai", key)),
                    (None, None) => {
                        Err(anyhow!("No API key found: set ANTHROPIC_API_KEY or OPENAI_API_KEY"))
                    }
                }?
            }
        };

        Ok(Self::for_provider(provider_name, api_key))
    }

    /// Build a client with an explicit provider name and API key.
    ///
    /// Base URL resolution:
    /// - anthropic: `ANTHROPIC_BASE_URL`
    /// - openai:    `OPENAI_BASE_URL`, falling back to `ANTHROPIC_BASE_URL`
    ///   (commonly set for OpenAI-compatible gateways like OpenRouter/poe)
    pub fn for_provider(provider_name: &str, api_key: String) -> Self {
        let base_url = std::env::var("ANTHROPIC_BASE_URL").ok();
        let provider: Arc<dyn LlmProvider> = match provider_name {
            "openai" => {
                let openai_base =
                    std::env::var("OPENAI_BASE_URL").or_else(|_| std::env::var("ANTHROPIC_BASE_URL"));
                Arc::new(OpenAiProvider::new(
                    api_key.clone(),
                    openai_base.ok(),
                ))
            }
            _ => Arc::new(AnthropicProvider::new(api_key.clone(), base_url)),
        };

        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(120))
            .pool_max_idle_per_host(4)
            .build()
            .expect("failed to build reqwest client");

        Self {
            provider,
            api_key,
            http,
            recorder: None,
            json_mode: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            input_tokens: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            output_tokens: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        }
    }

    /// Legacy constructor: provider from `LLM_PROVIDER` (default anthropic),
    /// key supplied explicitly.
    pub fn new(api_key: String) -> Self {
        let provider_name = std::env::var("LLM_PROVIDER").unwrap_or_else(|_| "anthropic".to_string());
        Self::for_provider(&provider_name, api_key)
    }

    /// Enable training data recording to the given file path (JSONL).
    pub fn with_training_recorder(mut self, path: impl AsRef<std::path::Path>) -> Self {
        self.recorder = Some(Arc::new(TrainingRecorder::new(path)));
        self
    }

    pub fn provider_name(&self) -> &'static str {
        self.provider.name()
    }

    pub fn model_name(&self) -> String {
        self.provider.model()
    }

    /// Swap the extraction model at runtime.
    pub fn set_model(&self, model: &str) {
        self.provider.set_model(model.to_string());
    }

    /// (input_tokens, output_tokens) accumulated across all calls.
    pub fn token_usage(&self) -> (u64, u64) {
        use std::sync::atomic::Ordering;
        (
            self.input_tokens.load(Ordering::Relaxed),
            self.output_tokens.load(Ordering::Relaxed),
        )
    }

    /// Second analytical pass: propose Resolves edges between known open
    /// questions and candidate answer nodes from a fresh extraction batch.
    ///
    /// Passive prose extraction reliably lands both the question and its
    /// answer in the graph but rarely links them; this pass closes that gap.
    /// Only exact IDs from the provided sets are accepted — hallucinated IDs
    /// are filtered here, not trusted.
    pub async fn link_resolutions(
        &self,
        questions: &[(String, String, String)],
        answers: &[(String, String, String)],
    ) -> Result<Vec<(String, String, String)>> {
        if questions.is_empty() || answers.is_empty() {
            return Ok(vec![]);
        }

        let mut user = String::from("=== OPEN QUESTIONS ===\n");
        for (id, label, desc) in questions {
            user.push_str(&format!("[{id}] {label} — {desc}\n"));
        }
        user.push_str("\n=== CANDIDATE ANSWER NODES ===\n");
        for (id, label, desc) in answers {
            user.push_str(&format!("[{id}] {label} — {desc}\n"));
        }
        user.push_str(
            "\nWhich candidate nodes answer which questions? Link whenever the answer addresses the question, even partially.",
        );

        let system = r#"You link answers to questions in a reasoning graph. Respond with ONLY raw JSON (no fences, no commentary):
{"new_nodes": [], "new_edges": [{"id": "<unique string>", "edge_type": "Resolves", "source_node_id": "<answer node id>", "target_node_id": "<question node id>", "reasoning": "<short why>", "timestamp": 0}]}
Use ONLY the exact node IDs given above. If nothing links, respond with {"new_nodes": [], "new_edges": []}."#;

        let body = self.provider.build_raw_json_body(system, &user);
        let raw = self.call_api(&body).await?;
        let result = self.provider.parse_extraction_response(raw.clone())?;
        debug!(
            "Linker response: {} edges proposed (from {} chars of content)",
            result.new_edges.len(),
            raw["choices"][0]["message"]["content"].as_str().map(|c| c.len()).unwrap_or(0)
        );

        let q_ids: HashSet<&str> = questions.iter().map(|(id, _, _)| id.as_str()).collect();
        let a_ids: HashSet<&str> = answers.iter().map(|(id, _, _)| id.as_str()).collect();
        let links: Vec<(String, String, String)> = result
            .new_edges
            .iter()
            .filter(|e| e.edge_type == EdgeType::Resolves)
            .filter(|e| q_ids.contains(e.target_node_id.as_str()))
            .filter(|e| a_ids.contains(e.source_node_id.as_str()))
            .map(|e| (e.target_node_id.clone(), e.source_node_id.clone(), e.reasoning.clone()))
            .collect();
        if links.is_empty() {
            if let Some(first) = result.new_edges.first() {
                warn!(
                    "Linker edges rejected by ID validation: known question ids {:?}, known answer ids {:?}, proposed source='{}' target='{}'",
                    q_ids,
                    a_ids,
                    truncate_for_log(&first.source_node_id),
                    truncate_for_log(&first.target_node_id),
                );
            } else {
                warn!(
                    "Linker proposed no edges; raw content: {}",
                    raw["choices"][0]["message"]["content"]
                        .as_str()
                        .map(truncate_for_log)
                        .unwrap_or_else(|| "(empty)".into())
                );
            }
        }
        Ok(links)
    }

    pub async fn extract(
        &self,
        event: &EventRecord,
        graph_snapshot: &GraphSnapshot,
        event_history: &[EventRecord],
        compressed_history: Option<&str>,
    ) -> Result<ExtractionResult> {
        self.extract_batch(
            std::slice::from_ref(event),
            graph_snapshot,
            event_history,
            compressed_history,
        )
        .await
        .map(|mut results| results.swap_remove(0))
    }

    /// Extract argument structure from a batch of events in a single LLM call.
    ///
    /// Returns one ExtractionResult per input event (in order); events with no
    /// argumentative content yield an empty result. Nodes and edges are
    /// validated against the current graph snapshot and the set of event IDs
    /// in the batch.
    pub async fn extract_batch(
        &self,
        events: &[EventRecord],
        graph_snapshot: &GraphSnapshot,
        event_history: &[EventRecord],
        compressed_history: Option<&str>,
    ) -> Result<Vec<ExtractionResult>> {
        if events.is_empty() {
            return Ok(vec![]);
        }

        // Skip events without enough signal; remember which made the cut so
        // we can return one result per input.
        let qualifying: Vec<&EventRecord> = events
            .iter()
            .filter(|e| e.has_extraction_signal(MIN_WORDS_FOR_EXTRACTION))
            .collect();
        if qualifying.is_empty() {
            return Ok(events.iter().map(|_| ExtractionResult::default()).collect());
        }

        let start = Instant::now();

        let user_msg = render_user_batch_extraction(
            &graph_snapshot.nodes,
            &graph_snapshot.edges,
            event_history,
            &qualifying.iter().copied().cloned().collect::<Vec<EventRecord>>(),
            compressed_history,
        );

        let base_system = render_system_batch_extraction();
        let system = format!("{}\n\n{}", base_system, budget_hint());
        let tool_schema = build_extraction_tool_schema();

        // Extraction call with self-healing: if the gateway ignores forced
        // tool use (empty content / parse failure), retry once in JSON mode
        // and remember the mode for all future batches.
        let use_json = self
            .json_mode
            .load(std::sync::atomic::Ordering::Relaxed);
        let body = if use_json {
            self.provider.build_json_extraction_body(&system, &user_msg)
        } else {
            self.provider
                .build_batch_extraction_body(&system, &user_msg, &tool_schema)
        };

        let raw = match self.call_api(&body).await {
            Ok(raw) => raw,
            Err(first_err) if !use_json => {
                warn!(
                    "Extraction call failed ({first_err}); retrying batch in JSON mode"
                );
                self.json_mode
                    .store(true, std::sync::atomic::Ordering::Relaxed);
                let json_body = self
                    .provider
                    .build_json_extraction_body(&system, &user_msg);
                self.call_api(&json_body).await?
            }
            Err(e) => return Err(e),
        };
        let duration = start.elapsed();

        // Parse the combined result, then attribute nodes/edges to the
        // qualifying events by their event_id field.
        let combined = self.provider.parse_extraction_response(raw.clone())?;
        let valid_event_ids: HashSet<&str> = qualifying.iter().map(|e| e.id.as_str()).collect();
        let result = self.validate(combined, graph_snapshot, &valid_event_ids);

        if let Some(recorder) = &self.recorder {
            recorder.record(TrainingRecord {
                id: uuid::Uuid::new_v4().to_string(),
                timestamp: chrono::Utc::now().to_rfc3339(),
                provider: self.provider.name().to_string(),
                model: self.provider.model(),
                operation: OperationType::Extraction,
                duration_ms: duration.as_millis() as u64,
                request_body: body,
                response_body: raw,
                parsed_result: serde_json::to_value(&result).unwrap_or_default(),
                extra: json!({
                    "event_ids": qualifying.iter().map(|e| e.id.clone()).collect::<Vec<_>>(),
                    "batch_size": qualifying.len(),
                    "session_id": qualifying.first().map(|e| e.session_id.clone()),
                }),
            });
        }

        // Split the validated result back into per-event buckets.
        let mut results: Vec<ExtractionResult> =
            events.iter().map(|_| ExtractionResult::default()).collect();
        let index_of: HashMap<&str, usize> = events
            .iter()
            .enumerate()
            .map(|(i, e)| (e.id.as_str(), i))
            .collect();
        for node in result.new_nodes {
            if let Some(&i) = index_of.get(node.event_id.as_str()) {
                results[i].new_nodes.push(node);
            }
        }
        for edge in result.new_edges {
            // Attribute an edge to the earliest event among its endpoints.
            // Edges to/from pre-existing snapshot nodes have no bucket of
            // their own; they ride with the first event that produced any
            // new node (the graph store validates the endpoints).
            let src_i = results.iter().position(|r| r.new_nodes.iter().any(|n| n.id == edge.source_node_id));
            let tgt_i = results.iter().position(|r| r.new_nodes.iter().any(|n| n.id == edge.target_node_id));
            let bucket = src_i
                .min(tgt_i)
                .or(src_i)
                .or(tgt_i)
                .or_else(|| results.iter().position(|r| !r.new_nodes.is_empty()));
            if let Some(i) = bucket {
                results[i].new_edges.push(edge);
            }
        }

        Ok(results)
    }

    pub async fn find_relevant_nodes(
        &self,
        question: &str,
        node_labels: &[(String, String)],
    ) -> Result<Vec<String>> {
        if node_labels.is_empty() {
            return Ok(vec![]);
        }

        let start = Instant::now();

        let system = "You are a graph search assistant. Return ONLY a JSON array of node ID strings, no other text.";
        let user_msg = render_user_find_nodes(question, node_labels);
        let body = self.provider.build_find_nodes_body(system, &user_msg);

        let raw = self.call_api(&body).await?;
        let duration = start.elapsed();
        let ids = self.provider.parse_find_nodes_response(raw.clone())?;

        let valid_ids: HashSet<&str> = node_labels.iter().map(|(id, _)| id.as_str()).collect();
        let filtered: Vec<String> = ids.into_iter().filter(|id| valid_ids.contains(id.as_str())).collect();

        if let Some(recorder) = &self.recorder {
            recorder.record(TrainingRecord {
                id: uuid::Uuid::new_v4().to_string(),
                timestamp: chrono::Utc::now().to_rfc3339(),
                provider: self.provider.name().to_string(),
                model: self.provider.model(),
                operation: OperationType::FindNodes,
                duration_ms: duration.as_millis() as u64,
                request_body: body,
                response_body: raw,
                parsed_result: serde_json::to_value(&filtered).unwrap_or_default(),
                extra: json!({ "question": question }),
            });
        }

        Ok(filtered)
    }

    pub async fn answer_query(
        &self,
        question: &str,
        subgraph: &GraphSnapshot,
    ) -> Result<QueryResult> {
        if subgraph.nodes.is_empty() {
            return Ok(QueryResult {
                answer: "The graph does not yet contain enough information to answer this question.".to_string(),
                trace: vec![],
                cited_events: vec![],
                mode_used: "llm".to_string(),
            });
        }

        let start = Instant::now();

        let query_tool = json!({
            "name": "provide_answer",
            "description": "Provide a structured answer to the user's question about the session reasoning graph.",
            "parameters": {
                "type": "object",
                "properties": {
                    "answer":            { "type": "string", "description": "Prose narrative answering the question, citing agents and session labels" },
                    "trace":             { "type": "array", "items": { "type": "string" }, "description": "Ordered list of node IDs forming the reasoning path" },
                    "cited_events":  { "type": "array", "items": { "type": "string" }, "description": "Event IDs referenced in the answer" }
                },
                "required": ["answer", "trace", "cited_events"]
            }
        });

        let user_msg = render_user_answer_query(question, subgraph);
        let system = "You are an argumentation expert answering questions about a session reasoning graph.";
        let body = self.provider.build_answer_body(system, &user_msg, &query_tool);

        let raw = self.call_api(&body).await?;
        let duration = start.elapsed();
        let result = self.provider.parse_answer_response(raw.clone())?;

        if let Some(recorder) = &self.recorder {
            recorder.record(TrainingRecord {
                id: uuid::Uuid::new_v4().to_string(),
                timestamp: chrono::Utc::now().to_rfc3339(),
                provider: self.provider.name().to_string(),
                model: self.provider.model(),
                operation: OperationType::AnswerQuery,
                duration_ms: duration.as_millis() as u64,
                request_body: body,
                response_body: raw,
                parsed_result: serde_json::to_value(&result).unwrap_or_default(),
                extra: json!({ "question": question }),
            });
        }

        Ok(result)
    }

    /// Compress a block of event text into a summary paragraph.
    pub async fn compress_text(&self, system: &str, user_msg: &str) -> Result<String> {
        let body = self.provider.build_find_nodes_body(system, user_msg);
        let raw = self.call_api(&body).await?;
        match self.provider.name() {
            "openai" => {
                Ok(raw["choices"][0]["message"]["content"]
                    .as_str()
                    .unwrap_or("(compression failed)")
                    .to_string())
            }
            _ => {
                Ok(raw["content"]
                    .as_array()
                    .and_then(|arr| arr.iter()
                        .find(|c| c["type"] == "text")
                        .and_then(|c| c["text"].as_str()))
                    .unwrap_or("(compression failed)")
                    .to_string())
            }
        }
    }

    /// Shared HTTP call: uses the provider's URL, headers, and retry logic.
    /// Accumulates token usage from the response `usage` block.
    async fn call_api(&self, body: &serde_json::Value) -> Result<serde_json::Value> {
        use std::time::Duration;

        let url = self.provider.request_url();
        let headers = self.provider.request_headers(&self.api_key);

        let mut delay = Duration::from_millis(500);
        for attempt in 0..3u32 {
            let mut req = self.http.post(&url);
            for (k, v) in &headers {
                req = req.header(k.as_str(), v.as_str());
            }
            let resp = req
                .json(body)
                .send()
                .await
                .map_err(|e| anyhow!("HTTP send failed: {e}"))?;

            let status = resp.status();
            if status.as_u16() == 429 {
                warn!("Rate limited (attempt {attempt}), retrying in {delay:?}");
                tokio::time::sleep(delay).await;
                delay *= 2;
                continue;
            }

            let text = resp.text().await.map_err(|e| anyhow!("failed to read response body: {e}"))?;
            if !status.is_success() {
                return Err(anyhow!("API error {status}: {text}"));
            }
            let parsed: serde_json::Value = serde_json::from_str(&text)
                .map_err(|e| anyhow!("failed to parse API response as JSON: {e}"))?;
            meter_usage(&self.input_tokens, &self.output_tokens, &parsed);
            return Ok(parsed);
        }
        Err(anyhow!("exhausted retries after rate limiting"))
    }

    fn validate(
        &self,
        mut result: ExtractionResult,
        snapshot: &GraphSnapshot,
        valid_event_ids: &HashSet<&str>,
    ) -> ExtractionResult {
        let existing_ids: HashSet<String> = snapshot.nodes.iter().map(|n| n.id.clone()).collect();
        let new_ids: HashSet<String> = result.new_nodes.iter().map(|n| n.id.clone()).collect();
        let all_known_ids: HashSet<&str> = existing_ids
            .iter()
            .chain(new_ids.iter())
            .map(|s| s.as_str())
            .collect();

        // ── Gate 1: Confidence clamp [0.1, 0.95] ────────────────────────────────
        for node in &mut result.new_nodes {
            node.confidence = node.confidence.clamp(0.1, 0.95);
        }

        // ── Gate 2: Provenance check ────────────────────────────────────────────
        // Every new node's event_id must belong to the batch being processed.
        // This prevents the LLM from hallucinating nodes that reference
        // non-existent or past events.
        let before = result.new_nodes.len();
        result.new_nodes.retain(|node| {
            if !valid_event_ids.contains(node.event_id.as_str()) {
                warn!(
                    "Dropping node {} — event_id '{}' not in current batch",
                    node.id, node.event_id
                );
                return false;
            }
            true
        });
        let dropped_nodes = before - result.new_nodes.len();
        if dropped_nodes > 0 {
            warn!("Validation dropped {dropped_nodes} nodes with mismatched provenance");
        }

        // ── Gate 3: Edge validation ─────────────────────────────────────────────
        let before = result.new_edges.len();
        result.new_edges.retain(|edge| {
            // 3a. No self-referential edges
            if edge.source_node_id == edge.target_node_id {
                warn!("Dropping self-referential edge: {}", edge.id);
                return false;
            }
            // 3b. Both endpoints must exist
            if !all_known_ids.contains(edge.source_node_id.as_str()) {
                warn!("Dropping edge {} — unknown source {}", edge.id, edge.source_node_id);
                return false;
            }
            if !all_known_ids.contains(edge.target_node_id.as_str()) {
                warn!("Dropping edge {} — unknown target {}", edge.id, edge.target_node_id);
                return false;
            }
            // 3c. Reasoning gate: mechanism description must be substantive
            let reasoning = edge.reasoning.trim();
            if reasoning.len() < 10 {
                warn!("Dropping edge {} — reasoning too short ({} chars)", edge.id, reasoning.len());
                return false;
            }
            // 3d. Reasoning must not be a restatement of node labels
            let reasoning_lower = reasoning.to_lowercase();
            let find_label = |node_id: &str| -> String {
                snapshot.nodes.iter()
                    .find(|n| n.id == node_id)
                    .map(|n| n.label.as_str())
                    .unwrap_or("")
                    .to_lowercase()
            };
            let src_lower = find_label(&edge.source_node_id);
            let tgt_lower = find_label(&edge.target_node_id);
            if reasoning_lower == src_lower || reasoning_lower == tgt_lower {
                warn!("Dropping edge {} — reasoning is exact restatement of a label", edge.id);
                return false;
            }
            if src_lower.len() >= 3 && tgt_lower.len() >= 3
                && reasoning_lower.contains(&src_lower)
                && reasoning_lower.contains(&tgt_lower)
                && reasoning_lower.len() <= src_lower.len() + tgt_lower.len() + 10
            {
                warn!("Dropping edge {} — reasoning is a concatenation of labels", edge.id);
                return false;
            }
            true
        });

        let dropped = before - result.new_edges.len();
        if dropped > 0 {
            warn!("Validation dropped {dropped} invalid edges");
        }

        // ── Gate 4: Provenance clamping ─────────────────────────────────────────
        for edge in &mut result.new_edges {
            let same_event_source = snapshot.nodes.iter().any(|n| {
                n.id == edge.source_node_id && valid_event_ids.contains(n.event_id.as_str())
            });
            let same_event_target = snapshot.nodes.iter().any(|n| {
                n.id == edge.target_node_id && valid_event_ids.contains(n.event_id.as_str())
            });

            // Both endpoints in the current event → likely explicit
            if same_event_source && same_event_target {
                edge.provenance = EdgeProvenance::Extracted;
            }
            // Very short reasoning → likely ambiguous
            if edge.reasoning.trim().len() < 20 {
                edge.provenance = EdgeProvenance::Ambiguous;
            }
        }

        result
    }
}
