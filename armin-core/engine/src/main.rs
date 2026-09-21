mod answer;
mod brief;
mod deterministic;
pub mod import;
pub mod toolclass;
mod metrics;
mod routes;
mod state;
mod worker;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use armin_extraction::{ExtractionClient, JevClient};
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

/// Resolve the generative LLM extraction client (Anthropic/OpenAI provider),
/// wiring the training-data recorder when configured.
fn resolve_llm_extractor(args: &Args) -> Option<Arc<ExtractionClient>> {
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
}

#[derive(Parser)]
#[command(name = "armin-engine", version)]
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

    /// Exit gracefully (sled flush) after this many seconds without any API
    /// request. Used by hook launchers that spawn the engine as a detached
    /// daemon: the next hook invocation simply starts a fresh one.
    #[arg(long)]
    idle_exit: Option<u64>,
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

    // Extraction setup. Jev (TypeSafe System One) is the default path; the
    // LLM extractor is the opt-out mode AND the automatic fallback when no
    // Typesafe key exists. Without any key: deterministic-only (tool-event
    // capture → scratch, agent writes, queries, analytics, import).
    let config = EngineConfig::from_env();
    let mut extractor: Option<Arc<ExtractionClient>> = None;
    let mut jev: Option<Arc<JevClient>> = None;

    if args.deterministic_only {
        info!("Extraction disabled (--deterministic-only)");
    } else {
        match config.extraction_mode {
            ExtractionMode::Jev => match armin_extraction::JevClient::from_env() {
                Ok(client) => jev = Some(Arc::new(client)),
                Err(e) => {
                    warn!(
                        "Jev extraction unavailable ({e}) — falling back to LLM \
                         extraction (set ARMIN_EXTRACTION_MODE=llm to silence)"
                    );
                    extractor = resolve_llm_extractor(&args);
                }
            },
            ExtractionMode::Llm => {
                extractor = resolve_llm_extractor(&args);
            }
        }
    }

    // In jev mode, an LLM client is still resolved when a key exists — for
    // /query answering only, never for extraction.
    if jev.is_some() && extractor.is_none() {
        extractor = armin_extraction::ExtractionClient::resolve()
            .ok()
            .map(Arc::new);
    }

    // Background extraction worker: prose events flow through this channel
    // and are batched before hitting the extractor.
    let (ingest_tx, ingest_rx) = mpsc::unbounded_channel::<armin_ingest::EventRecord>();

    let state = state::EngineState {
        graph: graph.clone(),
        retriever: retriever.clone(),
        extractor: extractor.clone(),
        jev: jev.clone(),
        ingest_tx: (extractor.is_some() || jev.is_some()).then_some(ingest_tx),
        scratch: toolclass::Scratch::new(),
        event_log: Arc::new(RwLock::new(std::collections::VecDeque::with_capacity(20))),
        sessions: Arc::new(RwLock::new(Vec::new())),
        current_session_idx: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        current_session_id: Arc::new(RwLock::new(String::new())),
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

    // Last-activity clock for --idle-exit: bumped by a middleware that wraps
    // every request, so any API call (ingest, brief, query, ...) counts.
    let last_activity = Arc::new(AtomicU64::new(unix_secs()));
    let (idle_tx, idle_rx) = tokio::sync::watch::channel(false);
    if let Some(idle_secs) = args.idle_exit {
        let last_activity = last_activity.clone();
        tokio::spawn(async move {
            let started = unix_secs();
            loop {
                tokio::time::sleep(Duration::from_secs(5)).await;
                let last = last_activity.load(Ordering::Relaxed).max(started);
                if unix_secs().saturating_sub(last) >= idle_secs {
                    info!("Idle for {idle_secs}s — shutting down (--idle-exit)");
                    let _ = idle_tx.send(true);
                    return;
                }
            }
        });
    }

    let auth_token: armin_http::AuthToken = armin_http::optional_token(args.auth_token);

    let app = Router::new()
        // ── Ingest & push ──
        .route("/api/v1/ingest", post(routes::ingest))
        .route("/api/v1/ingest/event", post(routes::ingest_one))
        .route("/api/v1/state/brief", get(routes::brief))
        .route("/api/v1/import", post(routes::import))
        .route("/ui", get(routes::ui))
        .route("/", get(routes::root_redirect))
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
        // No browser clients are expected: the engine is called server-side
        // by the harness (Bun). Deny all cross-origin requests.
        .layer(CorsLayer::new())
        .layer(middleware::from_fn_with_state(
            auth_token,
            armin_http::require_bearer,
        ))
        // Outermost layer: count every request (even auth failures) as
        // activity for --idle-exit.
        .layer(middleware::from_fn_with_state(
            last_activity.clone(),
            bump_activity,
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
        .with_graceful_shutdown(shutdown_signal(idle_rx))
        .await?;

    // Persist everything that is still in memory.
    graph.flush().await?;
    info!("Graph flushed; shutdown complete");

    Ok(())
}

fn unix_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Bump the idle clock on every request.
async fn bump_activity(
    axum::extract::State(last): axum::extract::State<Arc<AtomicU64>>,
    req: axum::extract::Request,
    next: middleware::Next,
) -> axum::response::Response {
    last.store(unix_secs(), Ordering::Relaxed);
    next.run(req).await
}

/// Resolve on ctrl-c (all platforms), SIGTERM (unix), or the --idle-exit
/// watcher's signal.
async fn shutdown_signal(mut idle: tokio::sync::watch::Receiver<bool>) {
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

    let idle_fired = async {
        loop {
            if idle.changed().await.is_err() {
                std::future::pending::<()>().await;
            }
            if *idle.borrow() {
                return;
            }
        }
    };

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
        _ = idle_fired => {},
    }
    info!("Shutdown signal received");
}
