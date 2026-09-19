mod answer;
mod brief;
mod deterministic;
mod metrics;
mod routes;
mod state;
mod worker;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;

use armin_graph::{Bm25Retriever, GraphStore, NodeRetriever};
use axum::Router;
use axum::middleware;
use axum::routing::{get, post};
use clap::Parser;
use tokio::sync::mpsc;
use tokio::sync::RwLock;
use tower_http::cors::CorsLayer;
use tracing::{info, warn};

use state::{EngineConfig, ExtractionMode};
use worker::run as run_worker;

#[derive(Parser)]
#[command(name = "armin-engine")]
struct Args {
    /// Port to listen on (0 = auto-assign; the chosen port is printed to
    /// stdout as ARMIN_PORT=<port> for the parent process).
    #[arg(long, default_value = "0")]
    port: u16,

    /// Bind address (default: loopback only).
    #[arg(long, default_value = "127.0.0.1")]
    bind: String,

    /// Path to sled database directory for persistent graph storage.
    #[arg(long)]
    db_path: Option<PathBuf>,

    /// Require this bearer token on every request. Recommended whenever the
    /// engine is spawned by a harness.
    #[arg(long)]
    auth_token: Option<String>,

    /// Extraction model override (defaults to LLM_MODEL or provider default).
    #[arg(long)]
    model: Option<String>,

    /// Disable LLM extraction entirely (graph grows only via /ingest tool
    /// events and explicit agent writes).
    #[arg(long)]
    deterministic_only: bool,

    /// Path to write LLM training data (JSONL).
    #[arg(long, env = "LLM_TRAINING_DATA_PATH")]
    training_data: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        // stdout carries the ARMIN_PORT handshake — logs must go to stderr.
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| {
                    tracing_subscriber::EnvFilter::new(
                        "armin_engine=info,armin_graph=warn,armin_extraction=warn",
                    )
                }),
        )
        .init();

    let args = Args::parse();
    if let Some(model) = &args.model {
        // Must be set before the provider is constructed; the provider reads
        // LLM_MODEL at build time.
        std::env::set_var("LLM_MODEL", model);
    }

    let graph = match &args.db_path {
        Some(path) => {
            // sled does not create parent directories itself.
            if let Some(parent) = path.parent() {
                if let Err(e) = std::fs::create_dir_all(parent) {
                    warn!("Failed to create db parent dir {}: {e}", parent.display());
                }
            }
            info!("Opening persistent graph database at {}", path.display());
            GraphStore::new_persistent(path.clone())?
        }
        None => {
            info!("Running with in-memory graph (no --db-path)");
            GraphStore::new()
        }
    };

    let retriever: Arc<dyn NodeRetriever> = Arc::new(Bm25Retriever::new(graph.clone()));

    // Extraction is optional: without an API key the engine still runs in
    // deterministic-only mode (tool events → Evidence nodes, agent writes,
    // queries, analytics).
    let config = EngineConfig::from_env();
    let extractor = if args.deterministic_only || config.extraction_mode == ExtractionMode::Jev {
        if config.extraction_mode == ExtractionMode::Jev {
            // Jev mode: prose extraction never touches the generative LLM.
            // The client may still be resolved for /query answering; if no
            // LLM key exists, query falls back to graph-only mode.
            match armin_extraction::ExtractionClient::resolve() {
                Ok(client) => Some(Arc::new(client)),
                Err(_) => None,
            }
        } else {
            info!("LLM extraction disabled (--deterministic-only)");
            None
        }
    } else {
        match armin_extraction::ExtractionClient::resolve() {
            Ok(client) => {
                let client = match &args.training_data {
                    Some(path) => client.with_training_recorder(path),
                    None => client,
                };
                Some(Arc::new(client))
            }
            Err(e) => {
                info!("Running deterministic-only: {e}");
                None
            }
        }
    };

    let jev = if config.extraction_mode == ExtractionMode::Jev {
        match armin_extraction::JevClient::from_env() {
            Ok(client) => Some(Arc::new(client)),
            Err(e) => {
                warn!("Jev extraction unavailable ({e}); prose events will be dropped");
                None
            }
        }
    } else {
        None
    };

    // Background extraction worker: prose events flow through this channel
    // and are batched before hitting the extractor.
    let (ingest_tx, ingest_rx) = mpsc::unbounded_channel::<armin_ingest::EventRecord>();

    let state = state::EngineState {
        graph: graph.clone(),
        retriever: retriever.clone(),
        extractor: extractor.clone(),
        jev: jev.clone(),
        ingest_tx: (extractor.is_some() || jev.is_some()).then_some(ingest_tx),
        event_log: Arc::new(RwLock::new(std::collections::VecDeque::with_capacity(20))),
        sessions: Arc::new(RwLock::new(Vec::new())),
        current_session_idx: Arc::new(AtomicUsize::new(0)),
        prior_debt: Arc::new(RwLock::new(None)),
        metrics: Arc::new(metrics::Metrics::default()),
        config,
        db_path: args.db_path.clone(),
    };
    state.metrics.started_at.store(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
        std::sync::atomic::Ordering::Relaxed,
    );

    if extractor.is_some() || jev.is_some() {
        tokio::spawn(run_worker(state.clone(), ingest_rx));
    }

    let auth_token: armin_http::AuthToken = armin_http::optional_token(args.auth_token);

    let app = Router::new()
        // ── Ingest & push ──
        .route("/api/v1/ingest", post(routes::ingest))
        .route("/api/v1/ingest/event", post(routes::ingest_one))
        .route("/api/v1/state/brief", get(routes::brief))
        // ── Graph mutations ──
        .route("/api/v1/nodes", post(routes::add_node))
        .route("/api/v1/edges", post(routes::add_edge))
        .route("/api/v1/extraction", post(routes::post_extraction))
        // ── Agent writes ──
        .route("/api/v1/agent/decision", post(routes::agent_decision))
        .route("/api/v1/agent/question", post(routes::agent_question))
        .route("/api/v1/invalidate", post(routes::invalidate))
        .route("/api/v1/resolve", post(routes::resolve))
        // ── Graph queries ──
        .route("/api/v1/snapshot", get(routes::snapshot))
        .route("/api/v1/recent", get(routes::recent))
        .route("/api/v1/find_nodes", post(routes::find_nodes))
        .route("/api/v1/bfs", post(routes::bfs))
        .route("/api/v1/query", post(routes::query))
        // ── Analytics ──
        .route("/api/v1/debt", get(routes::debt))
        .route("/api/v1/decisions", get(routes::decisions))
        .route("/api/v1/risks", get(routes::risks))
        .route("/api/v1/summary", get(routes::summary))
        .route("/api/v1/communities", get(routes::communities))
        .route("/api/v1/diff", get(routes::diff))
        // ── Ops ──
        .route("/api/v1/health", get(routes::health))
        .route("/api/v1/metrics", get(routes::metrics))
        .route("/api/v1/config", post(routes::update_config))
        // No browser clients are expected: the engine is called server-side
        // by the harness (Bun). Deny all cross-origin requests.
        .layer(CorsLayer::new())
        .layer(middleware::from_fn_with_state(
            auth_token,
            armin_http::require_bearer,
        ))
        .with_state(state);

    let addr = format!("{}:{}", args.bind, args.port);
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    let actual_port = listener.local_addr()?.port();
    info!("armin-engine listening on {addr}");

    // Handshake for the parent process (must be line 1 on stdout).
    println!("ARMIN_PORT={actual_port}");
    use std::io::Write;
    let _ = std::io::stdout().flush();

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    // Persist everything that is still in memory.
    graph.flush().await?;
    info!("Graph flushed; shutdown complete");

    Ok(())
}

/// Resolve on ctrl-c (all platforms) and SIGTERM (unix).
async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut sig) => {
                sig.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
    info!("Shutdown signal received");
}
