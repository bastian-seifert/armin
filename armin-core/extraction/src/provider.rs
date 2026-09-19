use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;
use armin_graph::{ExtractionResult, QueryResult};
use serde_json::Value;
use std::sync::Arc;

#[async_trait]
pub trait LlmProvider: Send + Sync {
    fn name(&self) -> &'static str;
    /// Current model id (cloned; the model can be swapped at runtime).
    fn model(&self) -> String;
    /// Swap the extraction model at runtime (e.g. harness pushes the
    /// live session model).
    fn set_model(&self, model: String);

    /// Build the HTTP request body for an extraction call.
    fn build_extraction_body(&self, system: &str, user_msg: &str, tool_schema: &Value) -> Value;

    /// Build the HTTP request body for a batch extraction call covering
    /// several events. Providers that support prompt caching should place a
    /// cache breakpoint on the (stable) system prompt so repeated batches hit
    /// the cache.
    fn build_batch_extraction_body(&self, system: &str, user_msg: &str, tool_schema: &Value) -> Value;

    /// Build a body for JSON-in-content extraction (no tools). Used as a
    /// fallback for gateways that ignore forced tool use.
    fn build_json_extraction_body(&self, system: &str, user_msg: &str) -> Value;

    /// Build a body for a custom JSON-task whose system prompt already
    /// carries its complete output spec (e.g. the resolution linker) — no
    /// extraction-format instruction is appended.
    fn build_raw_json_body(&self, system: &str, user_msg: &str) -> Value;

    /// Build the HTTP request body for a find-nodes call.
    fn build_find_nodes_body(&self, system: &str, user_msg: &str) -> Value;

    /// Build the HTTP request body for an answer-query call.
    fn build_answer_body(&self, system: &str, user_msg: &str, query_tool: &Value) -> Value;

    /// Extract ExtractionResult from a raw provider response.
    fn parse_extraction_response(&self, raw: Value) -> Result<ExtractionResult>;

    /// Extract node ID strings from a raw find-nodes response.
    fn parse_find_nodes_response(&self, raw: Value) -> Result<Vec<String>>;

    /// Extract QueryResult from a raw answer-query response.
    fn parse_answer_response(&self, raw: Value) -> Result<QueryResult>;

    /// Provider-specific request URL.
    fn request_url(&self) -> String;

    /// Provider-specific request headers (including auth).
    fn request_headers(&self, api_key: &str) -> Vec<(String, String)>;
}

/// Output token cap for JSON-mode extraction, scaled with the thoroughness
/// budget: high-budget prompts make reasoning models think longer, and
/// reasoning tokens share the completion budget — a fixed cap truncates.
fn json_max_tokens() -> u32 {
    match std::env::var("ARMIN_EXTRACT_BUDGET").unwrap_or_default().as_str() {
        "low" => 4096,
        "high" => 16384,
        _ => 8192,
    }
}

/// Shared instruction appended to the system prompt in JSON mode.
///
/// Granularity guidance is NOT hardcoded here — the client appends a
/// budget hint (`budget_hint()`) so thoroughness is tunable without
/// touching the providers.
pub const JSON_MODE_INSTRUCTION: &str = r#"OUTPUT FORMAT (strict): respond with ONLY a raw JSON object — no markdown fences, no commentary, no reasoning before the JSON — matching exactly:
{"new_nodes": [{"id": "<unique string>", "node_type": "<Claim|Evidence|Assumption|Question|Decision>", "label": "<max 12 words>", "description": "<ONE short sentence>", "event_id": "<the exact event id the node came from>", "agent_id": "<speaker>", "session_id": "<session id>", "timestamp": <number>, "confidence": <0.0-1.0>}], "new_edges": [{"id": "<unique string>", "edge_type": "<Supports|Contradicts|Refines|Resolves>", "source_node_id": "<node id>", "target_node_id": "<node id>", "reasoning": "<why, few words>", "timestamp": <number>}]}
Each node must trace to exactly one event. When a new node answers a question from the graph context or another event in this batch, add a Resolves edge (source = answer node, target = question node). If no event has argumentative content, respond with {"new_nodes": [], "new_edges": []}."#;

/// Extraction granularity hints, selected via `ARMIN_EXTRACT_BUDGET`.
/// Extraction is ~0.3% of session cost, so "medium" is the sensible default
/// and "high" is affordable for dense analytic conversations.
const BUDGET_HINTS: &[(&str, &str)] = &[
    (
        "low",
        "Be terse: short labels, one-sentence descriptions, 1-4 nodes per event.",
    ),
    (
        "medium",
        "Extract each distinct decision, question, assumption, and evidence item as its OWN node — never merge two items into one label. Up to 6 nodes per event.",
    ),
    (
        "high",
        "Extract EVERY distinct decision, question, assumption, and evidence item as its own node — never merge two items into one label. Surface implicit assumptions (things taken for granted without argument). Up to 12 nodes per event; descriptions may be two sentences. Prefer recall over brevity.",
    ),
];

/// Granularity hint for the extraction prompt, from `ARMIN_EXTRACT_BUDGET`
/// (low | medium | high; default medium).
pub fn budget_hint() -> &'static str {
    let level = std::env::var("ARMIN_EXTRACT_BUDGET").unwrap_or_default();
    BUDGET_HINTS
        .iter()
        .find(|(k, _)| *k == level)
        .map(|(_, v)| *v)
        .unwrap_or(BUDGET_HINTS[1].1)
}

/// Convert the Anthropic-shaped tool schema (`name`, `description`,
/// `input_schema`) into OpenAI function-calling shape (`parameters`).
fn anthropic_tool_schema_to_openai(schema: &Value) -> Value {
    let mut function = schema.clone();
    if let Some(obj) = function.as_object_mut() {
        if let Some(params) = obj.remove("input_schema") {
            obj.insert("parameters".to_string(), params);
        }
    }
    serde_json::json!({ "type": "function", "function": function })
}

/// Normalize a user-supplied base URL so appending `/v1/...` is safe:
/// strips trailing slashes and a trailing `/v1` (users frequently paste
/// base URLs that already include the version prefix).
fn normalize_base(base: &str) -> String {
    let mut b = base.trim().trim_end_matches('/').to_string();
    if b.ends_with("/v1") {
        b.truncate(b.len() - 3);
    }
    b
}

/// Strip markdown fences and surrounding prose from a JSON payload.
/// Handles: plain JSON, ```json ... ```, and prose-wrapped JSON (extracts
/// the first balanced top-level object/array).
fn strip_json_fences(text: &str) -> String {
    let trimmed = text.trim();
    let without_fences = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```"))
        .unwrap_or(trimmed)
        .strip_suffix("```")
        .unwrap_or(trimmed)
        .trim();

    // Fast path: already a JSON object/array.
    if (without_fences.starts_with('{') && without_fences.ends_with('}'))
        || (without_fences.starts_with('[') && without_fences.ends_with(']'))
    {
        return without_fences.to_string();
    }

    // Slow path: extract the first balanced JSON structure, ignoring
    // braces inside strings.
    let bytes: Vec<char> = without_fences.chars().collect();
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escape = false;
    let mut start: Option<usize> = None;
    for (i, c) in bytes.iter().enumerate() {
        if in_string {
            if escape {
                escape = false;
            } else if *c == '\\' {
                escape = true;
            } else if *c == '"' {
                in_string = false;
            }
            continue;
        }
        match c {
            '"' => in_string = true,
            '{' | '[' => {
                if depth == 0 {
                    start = Some(i);
                }
                depth += 1;
            }
            '}' | ']' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    if let Some(s) = start {
                        return bytes[s..=i].iter().collect();
                    }
                }
            }
            _ => {}
        }
    }
    without_fences.to_string()
}

pub(crate) fn truncate_for_log(s: &str) -> String {
    if s.chars().count() <= 200 {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(200).collect::<String>())
    }
}

/// Attempt to parse an extraction JSON payload; on EOF-style truncation
/// (model hit the output budget mid-JSON), salvage the complete leading
/// elements by finding the last object boundary that closes cleanly.
///
/// Node/edge objects are flat, so appending the missing array/object
/// closers to the last complete `}` reconstructs a valid payload.
fn try_parse_extraction_json(s: &str) -> Option<Value> {
    if let Ok(v) = serde_json::from_str::<Value>(s) {
        return Some(v);
    }
    let candidates = s
        .char_indices()
        .filter(|(_, c)| *c == '}')
        .map(|(i, _)| i)
        .collect::<Vec<_>>();
    // Walk object boundaries from the end; each candidate gets both possible
    // closings (inside "new_nodes"/"new_edges" array vs. root object).
    for &i in candidates.iter().rev().take(50) {
        for closer in ["]}", "}"] {
            let candidate = format!("{}{}", &s[..=i], closer);
            if let Ok(v) = serde_json::from_str::<Value>(&candidate) {
                if v.get("new_nodes").is_some() || v.get("new_edges").is_some() {
                    return Some(v);
                }
            }
        }
    }
    None
}

// ── Anthropic Provider ─────────────────────────────────────────────────────────

pub struct AnthropicProvider {
    messages_url: String,
    /// Shared so the model can be swapped at runtime without rebuilding the
    /// provider (the harness may push its live session model).
    model: Arc<std::sync::RwLock<String>>,
}

impl AnthropicProvider {
    pub fn new(api_key: String, base_url: Option<String>) -> Self {
        let _ = api_key;
        let base = base_url
            .map(|b| normalize_base(&b))
            .unwrap_or_else(|| "https://api.anthropic.com".to_string());
        let messages_url = format!("{base}/v1/messages");
        let model = Arc::new(std::sync::RwLock::new(
            std::env::var("LLM_MODEL").unwrap_or_else(|_| "claude-sonnet-4-6".to_string()),
        ));

        Self { messages_url, model }
    }

    fn current_model(&self) -> String {
        self.model.read().expect("model lock").clone()
    }

    fn find_tool_use(&self, raw: &Value) -> Result<Value> {
        let content = raw["content"]
            .as_array()
            .ok_or_else(|| anyhow!("missing content array"))?;
        let tool_use = content
            .iter()
            .find(|c| c["type"] == "tool_use")
            .ok_or_else(|| anyhow!("no tool_use block in response"))?;
        Ok(tool_use["input"].clone())
    }

    fn find_text_content(&self, raw: &Value) -> Result<String> {
        let content_arr = raw["content"]
            .as_array()
            .ok_or_else(|| anyhow!("missing content array"))?;
        let text = content_arr
            .iter()
            .find(|c| c["type"] == "text")
            .and_then(|c| c["text"].as_str())
            .ok_or_else(|| anyhow!("no text block in response"))?;
        Ok(text.to_string())
    }
}

#[async_trait]
impl LlmProvider for AnthropicProvider {
    fn name(&self) -> &'static str { "anthropic" }
    fn model(&self) -> String { self.current_model() }
    fn set_model(&self, model: String) { *self.model.write().expect("model lock") = model; }

    fn build_extraction_body(&self, system: &str, user_msg: &str, tool_schema: &Value) -> Value {
        serde_json::json!({
            "model": self.current_model(),
            "max_tokens": 2048,
            "system": system,
            "tools": [tool_schema],
            "tool_choice": { "type": "tool", "name": "extract_argument_nodes" },
            "messages": [{ "role": "user", "content": user_msg }]
        })
    }

    fn build_batch_extraction_body(&self, system: &str, user_msg: &str, tool_schema: &Value) -> Value {
        // Anthropic prompt caching: system prompt is stable across batches,
        // so mark it as a cache breakpoint.
        serde_json::json!({
            "model": self.current_model(),
            "max_tokens": 4096,
            "system": [{
                "type": "text",
                "text": system,
                "cache_control": { "type": "ephemeral" }
            }],
            "tools": [tool_schema],
            "tool_choice": { "type": "tool", "name": "extract_argument_nodes" },
            "messages": [{ "role": "user", "content": user_msg }]
        })
    }

    fn build_json_extraction_body(&self, system: &str, user_msg: &str) -> Value {
        serde_json::json!({
            "model": self.current_model(),
            "max_tokens": json_max_tokens(),
            "system": [{
                "type": "text",
                "text": format!("{system}\n\n{}", JSON_MODE_INSTRUCTION),
                "cache_control": { "type": "ephemeral" }
            }],
            "messages": [{ "role": "user", "content": user_msg }]
        })
    }

    fn build_raw_json_body(&self, system: &str, user_msg: &str) -> Value {
        serde_json::json!({
            "model": self.current_model(),
            "max_tokens": json_max_tokens(),
            "system": [{
                "type": "text",
                "text": system,
                "cache_control": { "type": "ephemeral" }
            }],
            "messages": [{ "role": "user", "content": user_msg }]
        })
    }

    fn build_find_nodes_body(&self, system: &str, user_msg: &str) -> Value {
        serde_json::json!({
            "model": self.current_model(),
            "max_tokens": 256,
            "system": system,
            "messages": [{ "role": "user", "content": user_msg }]
        })
    }

    fn build_answer_body(&self, system: &str, user_msg: &str, query_tool: &Value) -> Value {
        serde_json::json!({
            "model": self.current_model(),
            "max_tokens": 1024,
            "system": system,
            "tools": [query_tool],
            "tool_choice": { "type": "tool", "name": "provide_answer" },
            "messages": [{ "role": "user", "content": user_msg }]
        })
    }

    fn parse_extraction_response(&self, raw: Value) -> Result<ExtractionResult> {
        // Preferred path: forced tool use.
        if let Ok(input) = self.find_tool_use(&raw) {
            return serde_json::from_value(input).context("failed to parse ExtractionResult");
        }

        // Fallback: JSON-mode payload in the text block (JSON-mode retry or
        // gateways without function calling).
        let text = raw["content"]
            .as_array()
            .and_then(|arr| {
                arr.iter()
                    .find(|c| c["type"] == "text")
                    .and_then(|c| c["text"].as_str())
            })
            .unwrap_or("");
        if text.is_empty() {
            return Err(anyhow!("no tool_use and no text block in response"));
        }
        let cleaned = strip_json_fences(text);
        try_parse_extraction_json(&cleaned)
            .map_or_else(
                || {
                    Err(anyhow!(
                        "no tool_use and content is not valid JSON — content: {}",
                        truncate_for_log(text)
                    ))
                },
                |value| {
                    serde_json::from_value(value)
                        .context("failed to parse ExtractionResult from JSON content")
                },
            )
    }

    fn parse_find_nodes_response(&self, raw: Value) -> Result<Vec<String>> {
        let text = self.find_text_content(&raw)?;
        let cleaned = strip_json_fences(&text);
        serde_json::from_str::<Vec<String>>(&cleaned)
            .map_err(|e| anyhow!("failed to parse find_nodes JSON array: {e} — raw: {}", truncate_for_log(&text)))
    }

    fn parse_answer_response(&self, raw: Value) -> Result<QueryResult> {
        let input = self.find_tool_use(&raw)?;
        let answer = input["answer"].as_str().unwrap_or("No answer provided.").to_string();
        let trace = input["trace"]
            .as_array()
            .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
            .unwrap_or_default();
        let cited_events = input["cited_events"]
            .as_array()
            .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
            .unwrap_or_default();
        Ok(QueryResult { answer, trace, cited_events, mode_used: "llm".to_string() })
    }

    fn request_url(&self) -> String { self.messages_url.clone() }

    fn request_headers(&self, api_key: &str) -> Vec<(String, String)> {
        vec![
            ("x-api-key".to_string(), api_key.to_string()),
            ("anthropic-version".to_string(), "2023-06-01".to_string()),
        ]
    }
}

// ── OpenAI Provider ────────────────────────────────────────────────────────────

pub struct OpenAiProvider {
    url: String,
    /// Shared so the model can be swapped at runtime.
    model: Arc<std::sync::RwLock<String>>,
}

impl OpenAiProvider {
    pub fn new(api_key: String, base_url: Option<String>) -> Self {
        let _ = api_key;
        let base = base_url
            .map(|b| normalize_base(&b))
            .unwrap_or_else(|| "https://api.openai.com".to_string());
        let url = format!("{base}/v1/chat/completions");
        let model = Arc::new(std::sync::RwLock::new(
            std::env::var("LLM_MODEL").unwrap_or_else(|_| "gpt-4o".to_string()),
        ));

        Self { url, model }
    }

    fn current_model(&self) -> String {
        self.model.read().expect("model lock").clone()
    }

    fn find_tool_call(&self, raw: &Value) -> Result<Value> {
        let choice = raw["choices"][0]["message"].clone();
        let tool_calls = choice["tool_calls"]
            .as_array()
            .ok_or_else(|| anyhow!("no tool_calls in response"))?;
        let first = tool_calls
            .first()
            .ok_or_else(|| anyhow!("empty tool_calls array"))?;
        let args: Value = serde_json::from_str(
            first["function"]["arguments"].as_str().unwrap_or("{}"),
        )?;
        Ok(args)
    }

    fn find_text(&self, raw: &Value) -> Result<String> {
        raw["choices"][0]["message"]["content"]
            .as_str()
            .ok_or_else(|| anyhow!("no content in response"))
            .map(String::from)
    }
}

#[async_trait]
impl LlmProvider for OpenAiProvider {
    fn name(&self) -> &'static str { "openai" }
    fn model(&self) -> String { self.current_model() }
    fn set_model(&self, model: String) { *self.model.write().expect("model lock") = model; }

    fn build_extraction_body(&self, system: &str, user_msg: &str, tool_schema: &Value) -> Value {
        serde_json::json!({
            "model": self.current_model(),
            "max_tokens": 2048,
            "messages": [
                { "role": "system", "content": system },
                { "role": "user", "content": user_msg }
            ],
            "tools": [{
                "type": "function",
                "function": anthropic_tool_schema_to_openai(tool_schema)
            }],
            "tool_choice": { "type": "function", "function": { "name": "extract_argument_nodes" } }
        })
    }

    fn build_batch_extraction_body(&self, system: &str, user_msg: &str, tool_schema: &Value) -> Value {
        // OpenAI caches long stable prefixes automatically; same shape as the
        // single-event body with a larger output budget.
        serde_json::json!({
            "model": self.current_model(),
            "max_tokens": 4096,
            "messages": [
                { "role": "system", "content": system },
                { "role": "user", "content": user_msg }
            ],
            "tools": [{
                "type": "function",
                "function": anthropic_tool_schema_to_openai(tool_schema)
            }],
            "tool_choice": { "type": "function", "function": { "name": "extract_argument_nodes" } }
        })
    }

    fn build_json_extraction_body(&self, system: &str, user_msg: &str) -> Value {
        serde_json::json!({
            "model": self.current_model(),
            "max_tokens": json_max_tokens(),
            "messages": [
                { "role": "system", "content": format!("{system}\n\n{}", JSON_MODE_INSTRUCTION) },
                { "role": "user", "content": user_msg }
            ]
        })
    }

    fn build_raw_json_body(&self, system: &str, user_msg: &str) -> Value {
        serde_json::json!({
            "model": self.current_model(),
            "max_tokens": json_max_tokens(),
            "messages": [
                { "role": "system", "content": system },
                { "role": "user", "content": user_msg }
            ]
        })
    }

    fn build_find_nodes_body(&self, system: &str, user_msg: &str) -> Value {
        serde_json::json!({
            "model": self.current_model(),
            "max_tokens": 256,
            "messages": [
                { "role": "system", "content": system },
                { "role": "user", "content": user_msg }
            ]
        })
    }

    fn build_answer_body(&self, system: &str, user_msg: &str, query_tool: &Value) -> Value {
        serde_json::json!({
            "model": self.current_model(),
            "max_tokens": 1024,
            "messages": [
                { "role": "system", "content": system },
                { "role": "user", "content": user_msg }
            ],
            "tools": [{
                "type": "function",
                "function": query_tool
            }],
            "tool_choice": { "type": "function", "function": { "name": "provide_answer" } }
        })
    }

    fn parse_extraction_response(&self, raw: Value) -> Result<ExtractionResult> {
        // Preferred path: proper OpenAI function calling.
        if let Ok(args) = self.find_tool_call(&raw) {
            return serde_json::from_value(args)
                .context("failed to parse ExtractionResult from OpenAI response");
        }

        // Fallback: OpenAI-compatible gateways that ignore forced tool
        // choice (e.g. poe) may return the payload as plain JSON content.
        // Recover it from the message content, tolerating markdown fences
        // and salvaging truncated output.
        let content = raw["choices"][0]["message"]["content"]
            .as_str()
            .unwrap_or("");
        let cleaned = strip_json_fences(content);
        try_parse_extraction_json(&cleaned)
            .map_or_else(
                || {
                    Err(anyhow!(
                        "no tool_calls and content is not valid JSON — content: {}",
                        truncate_for_log(content)
                    ))
                },
                |value| {
                    serde_json::from_value(value)
                        .context("failed to parse ExtractionResult from JSON content")
                },
            )
    }

    fn parse_find_nodes_response(&self, raw: Value) -> Result<Vec<String>> {
        // Preferred: text content (works on all gateways).
        if let Ok(text) = self.find_text(&raw) {
            let cleaned = strip_json_fences(&text);
            if let Ok(ids) = serde_json::from_str::<Vec<String>>(&cleaned) {
                return Ok(ids);
            }
            return Err(anyhow!(
                "failed to parse find_nodes JSON: — raw: {}",
                truncate_for_log(&text)
            ));
        }
        // Fallback: JSON array inside tool_call content (unusual but valid).
        let args = self.find_tool_call(&raw)?;
        serde_json::from_value(args).map_err(|e| anyhow!("failed to parse find_nodes JSON: {e}"))
    }

    fn parse_answer_response(&self, raw: Value) -> Result<QueryResult> {
        let args = self.find_tool_call(&raw)?;
        let answer = args["answer"].as_str().unwrap_or("No answer provided.").to_string();
        let trace = args["trace"]
            .as_array()
            .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
            .unwrap_or_default();
        let cited_events = args["cited_events"]
            .as_array()
            .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
            .unwrap_or_default();
        Ok(QueryResult { answer, trace, cited_events, mode_used: "llm".to_string() })
    }

    fn request_url(&self) -> String { self.url.clone() }

    fn request_headers(&self, api_key: &str) -> Vec<(String, String)> {
        vec![
            ("Authorization".to_string(), format!("Bearer {api_key}")),
            ("Content-Type".to_string(), "application/json".to_string()),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_base_strips_version_and_slashes() {
        assert_eq!(normalize_base("https://api.poe.com/v1"), "https://api.poe.com");
        assert_eq!(normalize_base("https://api.poe.com/v1/"), "https://api.poe.com");
        assert_eq!(normalize_base("https://api.openai.com/"), "https://api.openai.com");
        assert_eq!(normalize_base("https://api.openai.com"), "https://api.openai.com");
        assert_eq!(normalize_base("  https://x.io/  "), "https://x.io");
    }

    #[test]
    fn strip_fences_plain_json() {
        assert_eq!(strip_json_fences(r#"{"a":1}"#), r#"{"a":1}"#);
    }

    #[test]
    fn strip_fences_removes_markdown() {
        let s = "```json\n{\"a\":1}\n```";
        assert_eq!(strip_json_fences(s), "{\"a\":1}");
    }

    #[test]
    fn strip_fences_extracts_from_prose() {
        let s = "Here is the JSON: {\"a\": \"b{\\\"c\\\"}\"} hope that helps";
        assert_eq!(strip_json_fences(s), r#"{"a": "b{\"c\"}"}"#);
    }

    #[test]
    fn strip_fences_handles_arrays() {
        let s = "prefix [\"id1\", \"id2\"] suffix";
        assert_eq!(strip_json_fences(s), "[\"id1\", \"id2\"]");
    }

    #[test]
    fn parse_extraction_falls_back_to_json_content() {
        let provider = OpenAiProvider::new("k".into(), None);
        let raw = serde_json::json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": "```json\n{\"new_nodes\":[{\"id\":\"n1\",\"node_type\":\"Decision\",\"label\":\"Use JWT\",\"description\":\"d\",\"event_id\":\"e1\",\"agent_id\":\"a\",\"session_id\":\"s\",\"timestamp\":1.0,\"confidence\":0.9}],\"new_edges\":[]}\n```"
                }
            }]
        });
        let result = provider.parse_extraction_response(raw).expect("parses");
        assert_eq!(result.new_nodes.len(), 1);
        assert_eq!(result.new_nodes[0].node_type, armin_graph::NodeType::Decision);
    }

    #[test]
    fn parse_extraction_still_prefers_tool_calls() {
        let provider = OpenAiProvider::new("k".into(), None);
        let raw = serde_json::json!({
            "choices": [{
                "message": {
                    "tool_calls": [{
                        "function": {
                            "arguments": "{\"new_nodes\":[],\"new_edges\":[]}"
                        }
                    }]
                }
            }]
        });
        let result = provider.parse_extraction_response(raw).expect("parses");
        assert!(result.new_nodes.is_empty());
    }

    #[test]
    fn salvage_recovers_truncated_json() {
        // Simulates a model that hit its output budget mid-node.
        let truncated = r#"{"new_nodes":[{"id":"n1","node_type":"Decision","label":"Use JWT","description":"d","event_id":"e1","agent_id":"a","session_id":"s","timestamp":1.0,"confidence":0.9},{"id":"n2","node_type":"Evid"#;
        let value = try_parse_extraction_json(truncated).expect("salvages");
        assert_eq!(value["new_nodes"].as_array().unwrap().len(), 1);
        assert_eq!(value["new_nodes"][0]["id"], "n1");
    }

    #[test]
    fn parse_find_nodes_tolerates_prose() {
        let provider = OpenAiProvider::new("k".into(), None);
        let raw = serde_json::json!({
            "choices": [{ "message": { "content": "The answer: [\"a\", \"b\"]" } }]
        });
        let ids = provider.parse_find_nodes_response(raw).expect("parses");
        assert_eq!(ids, vec!["a".to_string(), "b".to_string()]);
    }
}
