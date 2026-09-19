
use std::path::PathBuf;
use std::sync::Arc;

use armin_extraction::{ExtractionClient, JevClient, JevNativeConfig};
use armin_graph::{GraphStore, NodeRetriever};
use armin_ingest::EventRecord;
use tokio::sync::{RwLock, mpsc::UnboundedSender};

use crate::metrics::Metrics;

/// Which engine extracts prose into graph nodes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum ExtractionMode {
    /// TypeSafe System One (Jev) native pipeline: per-sentence typed
    /// judgments, verbatim nodes, pairwise edges. No generative LLM.
    /// THE DEFAULT: verbatim nodes are hallucination-proof by construction,
    /// and Jev judgment calls are cheaper than a generative extraction call.
    #[default]
    Jev,
    /// Opt-out: generative extraction LLM (poe/OpenAI/Anthropic). Also the
    /// automatic fallback when Jev is requested but no Typesafe key exists.
    Llm,
}

impl ExtractionMode {
    fn from_env() -> Self {
        match std::env::var("ARMIN_EXTRACTION_MODE")
            .unwrap_or_default()
            .to_ascii_lowercase()
            .as_str()
        {
            "llm" => Self::Llm,
            _ => Self::Jev, // default; "jev" and unset both land here
        }
    }
}

/// Tuning knobs for the extraction pipeline. Defaults are chosen so the
/// engine adds no measurable cost to the embedding agent: extraction runs in
/// the background, batched, on a cheap model.
#[derive(Clone, Debug)]
pub struct EngineConfig {
    /// Max wall-clock time to hold events before extracting (ms).
    pub batch_ms: u64,
    /// Max events per LLM extraction call.
    pub batch_events: usize,
    /// Events with fewer words than this are never sent to the LLM.
    pub min_words: usize,
    /// Convert ToolCall events to Evidence nodes without the LLM.
    pub deterministic_tools: bool,
    /// Run the resolution-linking pass after each extraction batch.
    pub linker: bool,
    /// Which extraction pipeline the worker runs.
    pub extraction_mode: ExtractionMode,
    /// Jev-native pipeline tuning knobs.
    pub jev_native: JevNativeConfig,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            batch_ms: 15_000,
            batch_events: 10,
            min_words: 5,
            deterministic_tools: true,
            linker: true,
            extraction_mode: ExtractionMode::default(),
            jev_native: JevNativeConfig::default(),
        }
    }
}

impl EngineConfig {
    /// Load overrides from the environment (all optional):
    /// `ARMIN_BATCH_MS`, `ARMIN_BATCH_EVENTS`, `ARMIN_MIN_WORDS`,
    /// `ARMIN_NO_DETERMINISTIC`, `ARMIN_NO_LINKER`,
    /// `ARMIN_EXTRACTION_MODE` (llm|jev) plus the `ARMIN_JEV_*` knobs.
    pub fn from_env() -> Self {
        let mut cfg = Self::default();
        if let Ok(v) = std::env::var("ARMIN_BATCH_MS") {
            if let Ok(n) = v.parse() {
                cfg.batch_ms = n;
            }
        }
        if let Ok(v) = std::env::var("ARMIN_BATCH_EVENTS") {
            if let Ok(n) = v.parse::<usize>() {
                cfg.batch_events = n.max(1);
            }
        }
        if let Ok(v) = std::env::var("ARMIN_MIN_WORDS") {
            if let Ok(n) = v.parse() {
                cfg.min_words = n;
            }
        }
        if std::env::var("ARMIN_NO_DETERMINISTIC").is_ok() {
            cfg.deterministic_tools = false;
        }
        if std::env::var("ARMIN_NO_LINKER").is_ok() {
            cfg.linker = false;
        }
        cfg.extraction_mode = ExtractionMode::from_env();
        cfg.jev_native = JevNativeConfig::from_env();
        // Thoroughness and batch size co-vary: output tokens ≈
        // events × nodes-per-event (+ reasoning tokens). A high extraction
        // budget on a large batch overflows the model's output window and
        // truncates the JSON. Halve the batch unless the caller pinned it.
        let high_budget =
            std::env::var("ARMIN_EXTRACT_BUDGET").unwrap_or_default() == "high";
        if high_budget && std::env::var("ARMIN_BATCH_EVENTS").is_err() {
            cfg.batch_events = (cfg.batch_events / 2).max(3);
        }
        cfg
    }
}

#[derive(Clone)]
pub struct EngineState {
    pub graph: GraphStore,
    pub retriever: Arc<dyn NodeRetriever>,
    /// None when running deterministic-only (no API key configured).
    /// Used for prose extraction in `Llm` mode and for /query answering.
    pub extractor: Option<Arc<ExtractionClient>>,
    /// Jev (System One) client for `Jev` extraction mode; None otherwise.
    pub jev: Option<Arc<JevClient>>,
    /// Sender into the background extraction worker. None when no extractor.
    pub ingest_tx: Option<UnboundedSender<EventRecord>>,
    /// Per-session scratch: recent tool mutations + verification outcomes.
    /// In-memory only; feeds cross-layer debt and the brief warnings.
    pub scratch: Arc<crate::toolclass::Scratch>,
    /// Rolling window of the last N events, used as LLM prompt context.
    pub event_log: Arc<RwLock<std::collections::VecDeque<EventRecord>>>,
    /// Insertion-ordered unique session IDs.
    pub sessions: Arc<RwLock<Vec<String>>>,
    pub current_session_idx: Arc<std::sync::atomic::AtomicUsize>,
    /// The most recently active session (scratch and briefs key off it).
    pub current_session_id: Arc<RwLock<String>>,
    /// Last computed debt report, used as the baseline for the next
    /// executive summary's debt delta.
    pub prior_debt: Arc<RwLock<Option<armin_graph::DebtReport>>>,
    pub config: EngineConfig,
    pub metrics: Arc<Metrics>,
    pub db_path: Option<PathBuf>,
}

/// Runtime-updatable settings (the harness pushes these, e.g. the live
/// session model).
#[derive(serde::Deserialize, Default)]
pub struct ConfigUpdate {
    /// Extraction model override.
    pub model: Option<String>,
}

impl EngineState {
    /// Track a session and set it as the current one (drives recency
    /// weighting in debt/risk scoring). Returns the session index.
    pub async fn register_session(&self, session_id: &str) -> usize {
        let idx = {
            let mut sessions = self.sessions.write().await;
            if !sessions.contains(&session_id.to_string()) {
                sessions.push(session_id.to_string());
            }
            sessions.len().saturating_sub(1)
        };
        self.current_session_idx
            .store(idx, std::sync::atomic::Ordering::SeqCst);
        *self.current_session_id.write().await = session_id.to_string();
        idx
    }

    /// The most recently active session ID (empty before the first ingest).
    pub async fn current_session_id(&self) -> String {
        self.current_session_id.read().await.clone()
    }

    /// Append an event to the rolling context window.
    pub async fn push_event_log(&self, event: EventRecord) {
        let mut log = self.event_log.write().await;
        if log.len() >= 20 {
            log.pop_front();
        }
        log.push_back(event);
    }

    /// Snapshot of recent conversation context for extraction prompts.
    pub async fn recent_history(&self) -> Vec<EventRecord> {
        self.event_log.read().await.iter().cloned().collect()
    }
}
