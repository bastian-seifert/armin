use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;

use anyhow::Context;
use armin_extraction::ExtractionClient;
use armin_graph::{Bm25Retriever, DebtReport, EmbeddingRetriever, GraphStore, HybridRetriever, NodeRetriever};
use armin_ingest::EventRecord;
use tokio::sync::{RwLock, broadcast};

use crate::pipeline::{
    stages::{CommunityStage, CompressionConfig, CompressionStage, DebtStage, ExtractionStage, GroundingStage},
    Pipeline,
};

#[derive(Clone)]
pub struct StreamMode {
    pub stream_path: PathBuf,
}

#[derive(Clone)]
pub struct AppState {
    pub graph: GraphStore,
    pub extractor: Arc<ExtractionClient>,
    pub retriever: Arc<dyn NodeRetriever>,
    pub pipeline: Arc<Pipeline>,
    /// Rolling window of last 20 events for LLM context
    pub event_log: Arc<RwLock<VecDeque<EventRecord>>>,
    /// Broadcast channel: all WS connections subscribe here
    pub ws_tx: broadcast::Sender<String>,
    pub current_session_idx: Arc<AtomicUsize>,
    pub prior_debt_report: Arc<RwLock<Option<DebtReport>>>,
    pub compressed_summary: Arc<RwLock<Option<String>>>,
}

impl AppState {
    pub fn new(
        api_key: String,
        db_path: Option<PathBuf>,
        brave_api_key: Option<String>,
        training_data_path: Option<PathBuf>,
        enable_compression: bool,
        compression_interval: usize,
        retriever_mode: &str,
    ) -> anyhow::Result<(Self, broadcast::Sender<String>)> {
        let graph = match db_path {
            Some(path) => {
                tracing::info!("Opening persistent graph database at {}", path.display());
                GraphStore::new_persistent(path).context("Failed to open persistent graph database")?
            }
            None => {
                tracing::info!("Running with in-memory graph (no --db-path)");
                GraphStore::new()
            }
        };

        let searcher = brave_api_key
            .map(|k| Arc::new(armin_grounding::BraveSearcher::new(k)) as Arc<dyn armin_grounding::EvidenceSearcher>);

        let retriever: Arc<dyn NodeRetriever> = match retriever_mode {
            "embedding" => Arc::new(EmbeddingRetriever::new(graph.clone(), api_key.clone())),
            "hybrid" => Arc::new(HybridRetriever::new(graph.clone(), api_key.clone())),
            _ => Arc::new(Bm25Retriever::new(graph.clone())),
        };

        let mut pipeline = Pipeline::new();
        if enable_compression {
            pipeline = pipeline.with_stage(Box::new(CompressionStage::new(CompressionConfig {
                interval: compression_interval,
                enabled: true,
            })));
        }
        pipeline = pipeline
            .with_stage(Box::new(ExtractionStage))
            .with_stage(Box::new(GroundingStage { searcher }))
            .with_stage(Box::new(DebtStage))
            .with_stage(Box::new(CommunityStage::new()));

        let (ws_tx, _) = broadcast::channel::<String>(1024);
        let state = Self {
            graph,
            extractor: {
                let client = ExtractionClient::new(api_key);
                let client = match &training_data_path {
                    Some(path) => {
                        tracing::info!("Recording LLM training data to {}", path.display());
                        client.with_training_recorder(path)
                    }
                    None => client,
                };
                Arc::new(client)
            },
            retriever,
            pipeline: Arc::new(pipeline),
            event_log: Arc::new(RwLock::new(VecDeque::with_capacity(20))),
            ws_tx: ws_tx.clone(),
            current_session_idx: Arc::new(AtomicUsize::new(0)),
            prior_debt_report: Arc::new(RwLock::new(None)),
            compressed_summary: Arc::new(RwLock::new(None)),
        };
        Ok((state, ws_tx))
    }
}
