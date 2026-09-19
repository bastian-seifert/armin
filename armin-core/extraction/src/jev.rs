//! Jev-native extraction via the TypeSafe System One API.
//!
//! Replaces the generative extraction LLM with typed judgments over the raw
//! conversation: every prose sentence gets a `choice` (its durable-knowledge
//! role: Decision, OpenItem, or noise). Kept sentences become nodes with
//! VERBATIM text, so hallucinated content is impossible by construction. A
//! second pairwise pass emits typed edges (Supersedes/Refutes/Resolves/
//! RelatesTo) with directional `choice`s.
//!
//! Thresholds are provisional for the minimal v2 criteria; Jev's calibration
//! is prompt-sensitive, so change the wording and the numbers together.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::{Result, anyhow};
use armin_graph::{
    ArgumentEdge, ArgumentNode, EdgeProvenance, EdgeType, ExtractionResult, NodeType, NodeStatus,
};
use armin_ingest::EventRecord;
use serde::Deserialize;
use serde_json::{Value, json};
use tracing::{debug, warn};

/// Max words in the generated label (schema says ~15; matches
/// deterministic.rs).
const MAX_LABEL_WORDS: usize = 12;
/// Max characters carried into the node description.
const MAX_DESCRIPTION_CHARS: usize = 400;
/// Choice confidence at or above which an edge counts as EXTRACTED.
const EXTRACTED_PROVENANCE_P: f64 = 0.8;

/// Durable-knowledge criteria (v2 taxonomy, minimal set).
///
/// Two categories, per the backtest/abtest evidence: Decisions are the
/// compaction payload; OpenItems keep unresolved threads alive. Everything
/// else is noise for now. Rule/Fact/Lesson criteria are planned extensions
/// once per-type gates are tuned — do not re-add without a gold set.
/// Wording is prompt-sensitive: change the wording and the thresholds
/// together.
pub const TYPE_CRITERIA: &[(&str, &str)] = &[
    (
        "Decision",
        "A settled choice about what to do, adopt, change, or reject in this codebase or task — including plans, approaches, and commitments that bind later work",
    ),
    (
        "OpenItem",
        "Something explicitly left unresolved or flagged for later — a TODO, an open question, deferred work, or a stated intention not yet acted on",
    ),
    (
        "noise",
        "Transient session content no future session needs: social filler, greetings, process meta-talk, restatements of the task, narration of moment-to-moment actions, status reports that expire with the session",
    ),
];

pub const EDGE_CRITERIA: &[(&str, &str)] = &[
    (
        "none",
        "No durable relation between the two items — unrelated, merely adjacent, or both transient",
    ),
    (
        "later_supersedes_earlier",
        "The later item replaces or overrides the earlier decision",
    ),
    (
        "later_resolves_earlier",
        "The later item settles or answers the earlier open item",
    ),
    (
        "later_relates_earlier",
        "The later item constrains, grounds, explains, or narrows the earlier one without contradicting it",
    ),
    (
        "earlier_supersedes_later",
        "The earlier item replaces or overrides the later decision",
    ),
    (
        "earlier_resolves_later",
        "The earlier item settles or answers the later open item",
    ),
    (
        "earlier_relates_later",
        "The earlier item constrains, grounds, explains, or narrows the later one without contradicting it",
    ),
];

const STOP_WORDS: &[&str] = &[
    "the", "a", "an", "and", "or", "of", "to", "in", "for", "on", "with", "at", "by", "from", "as",
    "is", "are", "was", "were", "be", "been", "being", "this", "that", "these", "those", "it",
    "its", "they", "them", "their", "we", "our", "you", "your", "i", "me", "my", "he", "she",
    "his", "her", "will", "would", "should", "could", "can", "may", "might", "shall", "must", "do",
    "does", "did", "not", "no", "nor", "so", "than", "too", "very", "just", "because", "but", "if",
    "while", "about", "what", "which", "who", "whom", "when", "where", "why", "how", "all", "any",
    "both", "each", "few", "more", "most", "other", "some", "such", "only", "own", "same", "then",
    "once", "here", "there", "also", "into", "out", "up", "down", "over", "under", "again",
    "further", "against", "between", "through", "during", "before", "after", "above", "below",
];

// ── Config ────────────────────────────────────────────────────────────────────

/// Tuning knobs for the Jev-native pipeline. Both kept types share
/// `decision_threshold`: their sentences are long and multi-clause, so Jev
/// hedges on phrasing and clusters in the mid-band. Defaults were tuned on
/// the OLD taxonomy backtest and are provisional for the minimal criteria —
/// retune against new gold.
#[derive(Clone, Debug)]
pub struct JevNativeConfig {
    /// Min probability of the chosen type for kept nodes (0.65).
    pub decision_threshold: f64,
    /// Min probability for keeping a pairwise edge (0.7).
    pub edge_threshold: f64,
    /// Min shared content tokens for a candidate edge pair (1).
    pub edge_min_overlap: usize,
    /// Min words for a sentence to be judged at all (6).
    pub min_sentence_words: usize,
    /// Cap on sentences judged per prose event (12).
    pub max_sentences_per_event: usize,
}

impl Default for JevNativeConfig {
    fn default() -> Self {
        Self {
            decision_threshold: 0.65,
            edge_threshold: 0.7,
            edge_min_overlap: 1,
            min_sentence_words: 6,
            max_sentences_per_event: 12,
        }
    }
}

impl JevNativeConfig {
    /// Optional env overrides: `ARMIN_JEV_DECISION_THRESHOLD`,
    /// `ARMIN_JEV_EDGE_THRESHOLD`, `ARMIN_JEV_EDGE_MIN_OVERLAP`,
    /// `ARMIN_JEV_MIN_SENTENCE_WORDS`, `ARMIN_JEV_MAX_SENTENCES`.
    pub fn from_env() -> Self {
        let mut cfg = Self::default();
        if let Ok(v) = std::env::var("ARMIN_JEV_DECISION_THRESHOLD") {
            if let Ok(f) = v.parse::<f64>() {
                cfg.decision_threshold = f.clamp(0.0, 1.0);
            }
        }
        if let Ok(v) = std::env::var("ARMIN_JEV_EDGE_THRESHOLD") {
            if let Ok(f) = v.parse::<f64>() {
                cfg.edge_threshold = f.clamp(0.0, 1.0);
            }
        }
        if let Ok(v) = std::env::var("ARMIN_JEV_EDGE_MIN_OVERLAP") {
            if let Ok(n) = v.parse::<usize>() {
                cfg.edge_min_overlap = n;
            }
        }
        if let Ok(v) = std::env::var("ARMIN_JEV_MIN_SENTENCE_WORDS") {
            if let Ok(n) = v.parse::<usize>() {
                cfg.min_sentence_words = n;
            }
        }
        if let Ok(v) = std::env::var("ARMIN_JEV_MAX_SENTENCES") {
            if let Ok(n) = v.parse::<usize>() {
                cfg.max_sentences_per_event = n.max(1);
            }
        }
        cfg
    }
}

// ── HTTP client ───────────────────────────────────────────────────────────────

/// Minimal TypeSafe System One client: one endpoint, bearer auth, retry
/// with backoff on 429/529/5xx, token metering.
pub struct JevClient {
    http: reqwest::Client,
    api_key: String,
    base_url: String,
    model: String,
    calls: AtomicU64,
    input_tokens: AtomicU64,
    output_tokens: AtomicU64,
}

#[derive(Debug, Deserialize)]
pub struct JevResponse {
    #[serde(default)]
    pub model: String,
    pub answers: HashMap<String, JevAnswer>,
    #[serde(default)]
    pub usage: JevUsage,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum JevAnswer {
    Choice {
        choice: String,
        #[serde(default)]
        probabilities: HashMap<String, f64>,
        #[serde(default)]
        confidence: f64,
    },
    Noul {
        noul: f64,
    },
    Score {
        score: f64,
    },
}

impl JevAnswer {
    /// For a Choice answer: (selected option, probability of that option).
    pub fn choice_probability(&self) -> Option<(&str, f64)> {
        match self {
            JevAnswer::Choice { choice, probabilities, .. } => {
                Some((choice.as_str(), probabilities.get(choice).copied().unwrap_or(0.0)))
            }
            _ => None,
        }
    }

    pub fn noul(&self) -> Option<f64> {
        match self {
            JevAnswer::Noul { noul } => Some(*noul),
            _ => None,
        }
    }
}

#[derive(Debug, Default, Deserialize)]
pub struct JevUsage {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
}

impl JevClient {
    /// Build a client from the environment. Requires `TYPESAFE_AI_API_KEY`
    /// (or `TYPESAFE_API_KEY`); optional `TYPESAFE_BASE_URL` and
    /// `ARMIN_JEV_MODEL` (default `jev-1.13.0`).
    pub fn from_env() -> Result<Self> {
        let api_key = std::env::var("TYPESAFE_AI_API_KEY")
            .or_else(|_| std::env::var("TYPESAFE_API_KEY"))
            .ok()
            .filter(|k| !k.trim().is_empty())
            .ok_or_else(|| anyhow!("no TypeSafe API key: set TYPESAFE_AI_API_KEY"))?;
        let base_url = std::env::var("TYPESAFE_BASE_URL")
            .unwrap_or_else(|_| "https://api.typesafe.ai".to_string());
        let model =
            std::env::var("ARMIN_JEV_MODEL").unwrap_or_else(|_| "jev-1.13.0".to_string());
        Ok(Self::new(api_key, base_url, model))
    }

    pub fn new(api_key: String, base_url: String, model: String) -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(120))
            .pool_max_idle_per_host(4)
            .build()
            .expect("failed to build reqwest client");
        Self {
            http,
            api_key,
            base_url: base_url.trim_end_matches('/').to_string(),
            model,
            calls: AtomicU64::new(0),
            input_tokens: AtomicU64::new(0),
            output_tokens: AtomicU64::new(0),
        }
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    /// (calls, input_tokens, output_tokens) accumulated so far.
    pub fn usage(&self) -> (u64, u64, u64) {
        (
            self.calls.load(Ordering::Relaxed),
            self.input_tokens.load(Ordering::Relaxed),
            self.output_tokens.load(Ordering::Relaxed),
        )
    }

    /// POST one System One request: `state` + typed `questions` → answers.
    /// Retries 429/529/5xx and transport errors with exponential backoff.
    pub async fn ask(&self, state: Value, questions: Value) -> Result<JevResponse> {
        let url = format!("{}/v1/systemone", self.base_url);
        let body = json!({ "state": state, "model": self.model, "questions": questions });

        let mut delay = Duration::from_millis(500);
        for attempt in 0..3u32 {
            if attempt > 0 {
                tokio::time::sleep(delay).await;
                delay *= 2;
            }
            let resp = self
                .http
                .post(&url)
                .bearer_auth(&self.api_key)
                .json(&body)
                .send()
                .await;

            let resp = match resp {
                Ok(r) => r,
                Err(e) => {
                    warn!("Jev request failed (attempt {attempt}): {e}");
                    continue;
                }
            };

            let status = resp.status();
            if status.as_u16() == 429 || status.as_u16() == 529 || status.is_server_error() {
                warn!("Jev API retryable status {status} (attempt {attempt})");
                continue;
            }
            if !status.is_success() {
                let text = resp.text().await.unwrap_or_default();
                return Err(anyhow!("Jev API error {status}: {}", truncate_for_log(&text, 300)));
            }

            let parsed: JevResponse = resp
                .json()
                .await
                .map_err(|e| anyhow!("failed to parse Jev response: {e}"))?;
            self.calls.fetch_add(1, Ordering::Relaxed);
            self.input_tokens
                .fetch_add(parsed.usage.input_tokens, Ordering::Relaxed);
            self.output_tokens
                .fetch_add(parsed.usage.output_tokens, Ordering::Relaxed);
            return Ok(parsed);
        }
        Err(anyhow!("Jev API exhausted retries"))
    }
}

fn truncate_for_log(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let cut: String = s.chars().take(max).collect();
    format!("{cut}…")
}

// ── Pure pipeline pieces (unit-testable, no HTTP) ─────────────────────────────

/// Split prose into sentences on terminal punctuation followed by a word
/// boundary. Word-based so string slicing always lands on char boundaries.
/// Abbreviations like "e.g." produce false splits; acceptable for judgment
/// inputs (each fragment still gets judged on its own text).
pub fn split_sentences(text: &str, min_words: usize, max_out: usize) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut current: Vec<&str> = Vec::new();
    for word in text.split_whitespace() {
        current.push(word);
        if word.ends_with('.') || word.ends_with('!') || word.ends_with('?') {
            let sentence = current.join(" ");
            current.clear();
            if sentence.split_whitespace().count() >= min_words {
                out.push(sentence);
                if out.len() >= max_out {
                    return out;
                }
            }
        }
    }
    if !current.is_empty() {
        let sentence = current.join(" ");
        if sentence.split_whitespace().count() >= min_words {
            out.push(sentence);
        }
    }
    out
}

pub fn parse_node_type(choice: &str) -> Option<NodeType> {
    match choice {
        "Decision" => Some(NodeType::Decision),
        "OpenItem" => Some(NodeType::OpenItem),
        _ => None,
    }
}

fn normalized(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_ascii_alphanumeric() || c.is_whitespace())
        .flat_map(|c| c.to_lowercase())
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Apply the node policy to one event's judged sentences:
/// `noise` → skip; otherwise keep only when the chosen type's probability
/// clears its gate (Decision and OpenItem share `decision_threshold`).
/// Exact-duplicate sentences within a batch are skipped.
pub fn build_native_nodes(
    event: &EventRecord,
    sentences: &[String],
    answers: &HashMap<String, JevAnswer>,
    cfg: &JevNativeConfig,
    seen: &mut HashSet<String>,
) -> Vec<ArgumentNode> {
    let mut nodes = Vec::new();
    for (i, sentence) in sentences.iter().enumerate() {
        let type_answer = match answers.get(&format!("s{i}_type")) {
            Some(a) => a,
            None => continue,
        };
        let (choice, p) = match type_answer.choice_probability() {
            Some(cp) => cp,
            None => continue,
        };
        if choice == "noise" {
            continue;
        }
        if p < cfg.decision_threshold {
            continue;
        }
        let node_type = match parse_node_type(choice) {
            Some(t) => t,
            None => continue,
        };
        let key = normalized(sentence);
        if key.is_empty() || !seen.insert(key) {
            continue;
        }

        let words: Vec<&str> = sentence.split_whitespace().collect();
        let label = if words.len() > MAX_LABEL_WORDS {
            format!("{}…", words[..MAX_LABEL_WORDS].join(" "))
        } else {
            sentence.clone()
        };
        let description = truncate_for_log(sentence, MAX_DESCRIPTION_CHARS);

        nodes.push(ArgumentNode {
            id: format!("jev-{}-s{i}", event.id),
            node_type,
            label,
            description,
            event_id: event.id.clone(),
            agent_id: "jev-native".to_string(),
            session_id: event.session_id.clone(),
            timestamp: event.end_time,
            confidence: p.clamp(0.1, 0.95) as f32,
            files: event.files.clone(),
            commit: event.commit.clone(),
            mention_count: 1,
            status: NodeStatus::Active,
        });
    }
    nodes
}

/// Content tokens with glue words removed, for the edge-pair prefilter.
pub fn content_tokens(text: &str) -> HashSet<String> {
    normalized(text)
        .split_whitespace()
        .filter(|w| !STOP_WORDS.contains(w))
        .map(|w| w.to_string())
        .collect()
}

/// Map a directional edge choice + probability to
/// (EdgeType, source node, target node) for an (earlier, later) pair.
pub fn map_relation<'a>(
    choice: &str,
    earlier: &'a ArgumentNode,
    later: &'a ArgumentNode,
) -> Option<(EdgeType, &'a ArgumentNode, &'a ArgumentNode)> {
    let (edge_type, source, target) = match choice {
        "later_supersedes_earlier" => (EdgeType::Supersedes, later, earlier),
        "later_refutes_earlier" => (EdgeType::Refutes, later, earlier),
        "later_resolves_earlier" => (EdgeType::Resolves, later, earlier),
        "later_relates_earlier" => (EdgeType::RelatesTo, later, earlier),
        "earlier_supersedes_later" => (EdgeType::Supersedes, earlier, later),
        "earlier_refutes_later" => (EdgeType::Refutes, earlier, later),
        "earlier_resolves_later" => (EdgeType::Resolves, earlier, later),
        "earlier_relates_later" => (EdgeType::RelatesTo, earlier, later),
        _ => return None,
    };
    Some((edge_type, source, target))
}

/// Chronological candidate pairs with at least one node new to this batch,
/// kept only when they share >= min_overlap content tokens. Returned as
/// (earlier, later, overlap).
pub fn candidate_pairs<'a>(
    new_nodes: &'a [ArgumentNode],
    recent_nodes: &'a [ArgumentNode],
    min_overlap: usize,
) -> Vec<(&'a ArgumentNode, &'a ArgumentNode, usize)> {
    let mut candidates: Vec<&ArgumentNode> = recent_nodes.iter().collect();
    candidates.extend(new_nodes.iter());
    candidates.sort_by(|a, b| {
        a.timestamp
            .partial_cmp(&b.timestamp)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.id.cmp(&b.id))
    });

    let texts: HashMap<&str, String> = candidates
        .iter()
        .map(|n| (n.id.as_str(), format!("{} {}", n.label, n.description)))
        .collect();
    let tokens: HashMap<&str, HashSet<String>> = candidates
        .iter()
        .map(|n| (n.id.as_str(), content_tokens(&texts[n.id.as_str()])))
        .collect();

    let new_ids: HashSet<&str> = new_nodes.iter().map(|n| n.id.as_str()).collect();
    let mut pairs = Vec::new();
    for (i, a) in candidates.iter().enumerate() {
        for b in &candidates[i + 1..] {
            if a.id == b.id {
                continue;
            }
            if !new_ids.contains(a.id.as_str()) && !new_ids.contains(b.id.as_str()) {
                continue;
            }
            let overlap = tokens[a.id.as_str()]
                .intersection(&tokens[b.id.as_str()])
                .count();
            if overlap >= min_overlap {
                pairs.push((*a, *b, overlap));
            }
        }
    }
    pairs
}

/// Build an ArgumentEdge from a judged pair, or None when the choice is
/// `none`/unknown or below the threshold.
#[allow(clippy::too_many_arguments)]
pub fn edge_from_answer(
    pair_index: usize,
    earlier: &ArgumentNode,
    later: &ArgumentNode,
    answer: Option<&JevAnswer>,
    cfg: &JevNativeConfig,
) -> Option<ArgumentEdge> {
    let (choice, p) = answer?.choice_probability()?;
    if choice == "none" || p < cfg.edge_threshold {
        return None;
    }
    let (edge_type, source, target) = map_relation(choice, earlier, later)?;
    Some(ArgumentEdge {
        id: format!("jev-e{pair_index}-{}", uuid::Uuid::new_v4()),
        edge_type,
        source_node_id: source.id.clone(),
        target_node_id: target.id.clone(),
        reasoning: format!("jev-native: {choice} (p={p:.2})"),
        timestamp: earlier.timestamp.max(later.timestamp),
        evidence_score: None,
        provenance: if p >= EXTRACTED_PROVENANCE_P {
            EdgeProvenance::Extracted
        } else {
            EdgeProvenance::Inferred {
                confidence: p as f32,
            }
        },
    })
}

// ── Request builders ─────────────────────────────────────────────────────────

fn criteria_map(criteria: &[(&str, &str)]) -> Value {
    Value::Object(
        criteria
            .iter()
            .map(|(k, v)| (k.to_string(), Value::String(v.to_string())))
            .collect(),
    )
}

fn sentence_request(event: &EventRecord, sentences: &[String]) -> (Value, Value) {
    let mut sentence_map = serde_json::Map::new();
    for (i, sentence) in sentences.iter().enumerate() {
        sentence_map.insert(format!("s{i}"), Value::String(sentence.clone()));
    }
    let state = json!({
        "event": event.text,
        "sentences": Value::Object(sentence_map),
    });

    let mut questions = serde_json::Map::new();
    for i in 0..sentences.len() {
        questions.insert(
            format!("s{i}_type"),
            json!({
                "type": "choice",
                "instructions": format!(
                    "A coding agent worked in this codebase and wrote \
                     `sentences.s{i}` inside the surrounding discussion in `event`. \
                     What durable knowledge does the sentence carry — what would a \
                     FUTURE session working in this codebase need to remember from \
                     it? Judge the sentence itself. Sentences that only narrate the \
                     moment (actions taken, readings, transitions, social filler, \
                     process talk, status that expires with the session, reports \
                     of work already completed — the committed code itself is the \
                     record of what was done) are noise: nothing in them will \
                     still matter once the session ends."
                ),
                "criteria": criteria_map(TYPE_CRITERIA),
            }),
        );
    }
    (state, Value::Object(questions))
}

fn edge_request(chunk: &[(&ArgumentNode, &ArgumentNode, usize)]) -> (Value, Value) {
    let mut pair_map = serde_json::Map::new();
    for (k, (earlier, later, _)) in chunk.iter().enumerate() {
        pair_map.insert(
            format!("p{k}"),
            json!({
                "earlier": format!("{} {}", earlier.label, earlier.description),
                "later": format!("{} {}", later.label, later.description),
            }),
        );
    }
    let state = json!({ "pairs": Value::Object(pair_map) });

    let mut questions = serde_json::Map::new();
    for k in 0..chunk.len() {
        questions.insert(
            format!("p{k}_relation"),
            json!({
                "type": "choice",
                "instructions": format!(
                    "In `pairs.p{k}`, does one of the two items bear a durable \
                     relation to the other — would knowing one change how a future \
                     session treats the other? `pairs.p{k}.earlier` occurred before \
                     `pairs.p{k}.later` in the conversation. Pick the single best \
                     description of how they relate, or none if they are unrelated \
                     or merely adjacent."
                ),
                "criteria": criteria_map(EDGE_CRITERIA),
            }),
        );
    }
    (state, Value::Object(questions))
}

// ── Orchestration ─────────────────────────────────────────────────────────────

/// Jev-native extraction for a batch of prose events:
/// per-event sentence scan → verbatim nodes, then one pairwise edge pass
/// over the new nodes (plus recent graph nodes as earlier-side context).
pub async fn extract_native_batch(
    client: &JevClient,
    batch: &[EventRecord],
    recent_nodes: &[ArgumentNode],
    cfg: &JevNativeConfig,
) -> Result<ExtractionResult> {
    let started = std::time::Instant::now();
    let mut seen: HashSet<String> = HashSet::new();
    let mut nodes: Vec<ArgumentNode> = Vec::new();

    for event in batch {
        let sentences =
            split_sentences(&event.text, cfg.min_sentence_words, cfg.max_sentences_per_event);
        if sentences.is_empty() {
            continue;
        }
        let (state, questions) = sentence_request(event, &sentences);
        let resp = client.ask(state, questions).await?;
        nodes.extend(build_native_nodes(event, &sentences, &resp.answers, cfg, &mut seen));
    }

    let mut edges: Vec<ArgumentEdge> = Vec::new();
    let mut edge_keys: HashSet<(String, String, String)> = HashSet::new();
    let pairs = candidate_pairs(&nodes, recent_nodes, cfg.edge_min_overlap);
    if !pairs.is_empty() {
        const EDGE_CHUNK: usize = 15;
        for chunk in pairs.chunks(EDGE_CHUNK) {
            let (state, questions) = edge_request(chunk);
            let resp = client.ask(state, questions).await?;
            for (k, (earlier, later, _)) in chunk.iter().enumerate() {
                let answer = resp.answers.get(&format!("p{k}_relation"));
                if let Some(edge) =
                    edge_from_answer(k, earlier, later, answer, cfg)
                {
                    let key = (
                        edge.source_node_id.clone(),
                        edge.target_node_id.clone(),
                        format!("{:?}", edge.edge_type),
                    );
                    if edge_keys.insert(key) {
                        edges.push(edge);
                    }
                }
            }
        }
    }

    debug!(
        "Jev-native extraction: {} events → {} nodes, {} edges in {:?}",
        batch.len(),
        nodes.len(),
        edges.len(),
        started.elapsed()
    );
    Ok(ExtractionResult {
        new_nodes: nodes,
        new_edges: edges,
    })
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use armin_ingest::EventKind;

    fn event(id: &str, text: &str) -> EventRecord {
        EventRecord {
            id: id.to_string(),
            session_id: "s1".to_string(),
            agent_role: "assistant".to_string(),
            start_time: 1.0,
            end_time: 2.0,
            text: text.to_string(),
            event_kind: EventKind::Utterance,
            tool_name: None,
            files: vec![],
            commit: None,
        }
    }

    fn choice_answer(choice: &str, p: f64) -> JevAnswer {
        JevAnswer::Choice {
            choice: choice.to_string(),
            probabilities: HashMap::from([("Claim".into(), 0.0), (choice.into(), p)]),
            confidence: p,
        }
    }

    #[test]
    fn split_sentences_respects_min_words_and_cap() {
        let text = "This sentence has enough words to pass. Short one. Another long \
                    enough sentence follows here! Final fragment without terminal \
                    punctuation that keeps going";
        let out = split_sentences(text, 6, 10);
        assert_eq!(out.len(), 3);
        assert!(out[0].starts_with("This sentence"));
        assert!(out[2].ends_with("keeps going"));

        let capped = split_sentences("One. Two. Three. Four. Five. Six.", 1, 3);
        assert_eq!(capped.len(), 3);
    }

    #[test]
    fn parse_answers_from_json() {
        let choice: JevAnswer = serde_json::from_value(json!({
            "type": "choice", "choice": "Evidence",
            "probabilities": {"Evidence": 0.96, "Claim": 0.04}, "confidence": 0.95
        }))
        .unwrap();
        assert_eq!(choice.choice_probability(), Some(("Evidence", 0.96)));

        let noul: JevAnswer = serde_json::from_value(json!({"type": "noul", "noul": 0.71})).unwrap();
        assert_eq!(noul.noul(), Some(0.71));
    }

    #[test]
    fn node_policy_noise_threshold_dedup() {
        let cfg = JevNativeConfig::default();
        let ev = event("f1", "irrelevant");
        let sentences = vec![
            "Thanks everyone for the great feedback today".to_string(),
            "The opencode config schema strips unknown top-level keys".to_string(),
            "Smoke test verified that auth returns 401 without a token".to_string(),
            "A cheap flash-class model is adequate for structured extraction quality"
                .to_string(),
            "The opencode config schema strips unknown top-level keys".to_string(),
        ];
        let answers: HashMap<String, JevAnswer> = HashMap::from([
            ("s0_type".to_string(), choice_answer("noise", 0.99)),
            ("s1_type".to_string(), choice_answer("Decision", 0.90)),
            ("s2_type".to_string(), choice_answer("OpenItem", 0.70)),
            ("s3_type".to_string(), choice_answer("OpenItem", 0.55)),
        ]);
        let mut seen = HashSet::new();
        let nodes = build_native_nodes(&ev, &sentences, &answers, &cfg, &mut seen);

        // s0 is noise, s1 (0.90) and s2 (0.70) clear the 0.65 gate, s3 (0.55)
        // does not, and the duplicate of s1 is deduped.
        assert_eq!(nodes.len(), 2);
        assert_eq!(nodes[0].node_type, NodeType::Decision);
        assert!((nodes[0].confidence - 0.90).abs() < 1e-6);
        assert_eq!(nodes[0].id, "jev-f1-s1");
        assert_eq!(nodes[1].node_type, NodeType::OpenItem);
        assert!((nodes[1].confidence - 0.70).abs() < 1e-6);
        // 8 words, no truncation marker.
        assert!(!nodes[0].label.ends_with('…'));
        assert!(!nodes[0].description.contains('\n'));
    }

    #[test]
    fn per_type_threshold_gates() {
        let cfg = JevNativeConfig::default();
        let ev = event("f2", "irrelevant");
        let sentences = vec![
            "Settled on the middleware architecture after weighing the alternatives".to_string(),
            "The metrics endpoint exposes runtime counters for operators".to_string(),
            "Is the storage backend migration scheduled for phase three".to_string(),
        ];
        let answers: HashMap<String, JevAnswer> = HashMap::from([
            ("s0_type".to_string(), choice_answer("Decision", 0.70)),
            ("s1_type".to_string(), choice_answer("OpenItem", 0.66)),
            ("s2_type".to_string(), choice_answer("Decision", 0.60)),
        ]);
        let mut seen = HashSet::new();
        let nodes = build_native_nodes(&ev, &sentences, &answers, &cfg, &mut seen);
        // Both types share the 0.65 gate: 0.70 and 0.66 clear it, 0.60 does not.
        assert_eq!(nodes.len(), 2);
        assert_eq!(nodes[0].node_type, NodeType::Decision);
        assert_eq!(nodes[1].node_type, NodeType::OpenItem);
    }

    #[test]
    fn content_tokens_drop_stop_words() {
        let a = content_tokens("The engine and the graph are solid");
        let b = content_tokens("The engine and the model are solid");
        assert_eq!(a.intersection(&b).count(), 2);
        assert!(a.contains("engine"));
        assert!(!a.contains("the"));
    }

    #[test]
    fn candidate_pairs_order_and_overlap() {
        let old = ArgumentNode {
            id: "old".into(),
            node_type: NodeType::Decision,
            label: "Use loopback binding for the engine".into(),
            description: "Use loopback binding".into(),
            event_id: "e0".into(),
            agent_id: "a".into(),
            session_id: "s1".into(),
            timestamp: 1.0,
            confidence: 0.9,
            files: vec![],
            commit: None,
            mention_count: 1,
            status: NodeStatus::Active,
        };
        let fresh = ArgumentNode {
            id: "new".into(),
            node_type: NodeType::Decision,
            label: "Bind the engine to loopback only".into(),
            description: "Bind the engine to loopback only".into(),
            event_id: "e1".into(),
            agent_id: "a".into(),
            session_id: "s1".into(),
            timestamp: 5.0,
            confidence: 0.9,
            files: vec![],
            commit: None,
            mention_count: 1,
            status: NodeStatus::Active,
        };
        let recent = [old.clone()];
        let fresh_slice = [fresh.clone()];
        let pairs = candidate_pairs(&fresh_slice, &recent, 1);
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].0.id, "old");
        assert_eq!(pairs[0].1.id, "new");
        assert!(pairs[0].2 >= 1);

        // Pairs entirely within the recent set are never re-judged.
        let empty: [ArgumentNode; 0] = [];
        assert!(candidate_pairs(&empty, &[old, fresh], 1).is_empty());
    }

    #[test]
    fn edge_mapping_direction_and_provenance() {
        let cfg = JevNativeConfig::default();
        let earlier = event("q1", "earlier");
        let later = event("a1", "later");
        let q_node = ArgumentNode {
            id: "q".into(),
            node_type: NodeType::OpenItem,
            label: "Does the gateway support forced tool calls?".into(),
            description: "Does the gateway support forced tool calls?".into(),
            event_id: "q1".into(),
            agent_id: "a".into(),
            session_id: "s1".into(),
            timestamp: 1.0,
            confidence: 0.9,
            files: vec![],
            commit: None,
            mention_count: 1,
            status: NodeStatus::Active,
        };
        let a_node = ArgumentNode {
            id: "a".into(),
            node_type: NodeType::Decision,
            label: "No, it ignores forced tool calls".into(),
            description: "No, it ignores forced tool calls".into(),
            event_id: "a1".into(),
            agent_id: "a".into(),
            session_id: "s1".into(),
            timestamp: 9.0,
            confidence: 0.9,
            files: vec![],
            commit: None,
            mention_count: 1,
            status: NodeStatus::Active,
        };
        let _ = (earlier, later);

        let answer = choice_answer("later_resolves_earlier", 0.95);
        let edge = edge_from_answer(0, &q_node, &a_node, Some(&answer), &cfg).unwrap();
        assert_eq!(edge.edge_type, EdgeType::Resolves);
        assert_eq!(edge.source_node_id, "a");
        assert_eq!(edge.target_node_id, "q");
        assert!(matches!(edge.provenance, EdgeProvenance::Extracted));

        let answer = choice_answer("later_relates_earlier", 0.75);
        let edge = edge_from_answer(1, &q_node, &a_node, Some(&answer), &cfg).unwrap();
        assert_eq!(edge.edge_type, EdgeType::RelatesTo);
        assert!(matches!(edge.provenance, EdgeProvenance::Inferred { .. }));

        assert!(edge_from_answer(2, &q_node, &a_node, Some(&choice_answer("none", 0.99)), &cfg).is_none());
        assert!(edge_from_answer(3, &q_node, &a_node, None, &cfg).is_none());
    }

    #[test]
    fn request_keys_match_answer_lookup() {
        let ev = event("f9", "a sentence here. another sentence follows!");
        let sentences = split_sentences(&ev.text, 2, 10);
        assert_eq!(sentences.len(), 2);
        let (state, questions) = sentence_request(&ev, &sentences);
        assert!(state["sentences"]["s0"].is_string());
        assert!(state["sentences"]["s1"].is_string());
        for i in 0..sentences.len() {
            assert!(questions.get(format!("s{i}_type")).is_some());
        }
        assert!(questions.get("s2_type").is_none());
    }
}
