mod answer;
mod mcp;
mod pipeline;
mod routes;
mod state;
mod stream_runner;
mod ws;

use std::path::PathBuf;

use axum::Router;
use axum::middleware;
use axum::routing::{any_service, get, post};
use clap::Parser;
use rmcp::transport::streamable_http_server::session::never::NeverSessionManager;
use rmcp::transport::{StreamableHttpServerConfig, StreamableHttpService};
use rmcp::ServiceExt;
use std::sync::Arc;
use tower_http::cors::CorsLayer;
use tracing::info;

use state::AppState;

#[derive(Parser)]
#[command(name = "armin-server")]
struct Args {
    /// Path to stream.jsonl (live LLM mode). Optional: without it the server
    /// still serves /ingest-driven sessions and all query endpoints.
    #[arg(long)]
    stream: Option<PathBuf>,

    /// Playback speed multiplier (0 = max speed, 1 = realtime, 2/4/8 = faster)
    #[arg(long, default_value = "4")]
    speed: u32,

    /// Bind address (default: loopback only — the graph is sensitive).
    #[arg(long, default_value = "127.0.0.1")]
    bind: String,

    /// Port to listen on
    #[arg(long, default_value = "8080")]
    port: u16,

    /// Require this bearer token on every request (off by default for the
    /// local demo; enable when exposing beyond loopback).
    #[arg(long, env = "ARMIN_AUTH_TOKEN")]
    auth_token: Option<String>,

    /// Path to sled database directory for persistent graph storage.
    #[arg(long)]
    db_path: Option<PathBuf>,

    /// Path to write LLM training data (JSONL). Also reads LLM_TRAINING_DATA_PATH env.
    #[arg(long, env = "LLM_TRAINING_DATA_PATH")]
    training_data: Option<PathBuf>,

    /// Enable history compression (summarizes old events before LLM extraction).
    #[arg(long)]
    enable_compression: bool,

    /// Number of events between compression invocations (default 5).
    #[arg(long, default_value = "5")]
    compression_interval: usize,

    /// Retrieval strategy: bm25, embedding, or hybrid (default: bm25).
    #[arg(long, default_value = "bm25")]
    retriever: String,

    /// Also serve MCP over stdio (for local agent integration alongside HTTP MCP).
    #[arg(long)]
    mcp_stdio: bool,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("armin_server=info".parse()?)
                .add_directive("armin_graph=warn".parse()?)
                .add_directive("armin_extraction=info".parse()?),
        )
        .init();

    let args = Args::parse();

    let api_key = std::env::var("ANTHROPIC_API_KEY")
        .or_else(|_| std::env::var("OPENAI_API_KEY"))
        .map_err(|_| anyhow::anyhow!("Set ANTHROPIC_API_KEY or OPENAI_API_KEY"))?;

    let stream_mode = args.stream.map(|path| {
        info!("Running in live mode from {}", path.display());
        crate::state::StreamMode { stream_path: path }
    });

    let brave_api_key = std::env::var("BRAVE_SEARCH_API_KEY").ok();

    let (state, _ws_tx) = AppState::new(
        api_key,
        args.db_path.clone(),
        brave_api_key,
        args.training_data,
        args.enable_compression,
        args.compression_interval,
        &args.retriever,
    )?;

    // Spawn stream runner in background (only when a stream is configured)
    if let Some(stream_mode) = &stream_mode {
        let runner_state = state.clone();
        let speed = args.speed;
        let path = stream_mode.stream_path.clone();
        tokio::spawn(async move {
            if let Err(e) = stream_runner::run_stream(runner_state, path, speed).await {
                tracing::error!("Stream runner error: {e}");
            }
        });
    }

    // Spawn stdio MCP server if requested
    if args.mcp_stdio {
        let mcp_state = state.clone();
        tokio::spawn(async move {
            let handler = crate::mcp::ArminMcpHandler::new(mcp_state);
            match handler.serve(rmcp::transport::io::stdio()).await {
                Ok(service) => {
                    if let Err(e) = service.waiting().await {
                        tracing::error!("MCP stdio server error: {e}");
                    }
                }
                Err(e) => tracing::error!("MCP stdio server setup error: {e}"),
            }
        });
    }

    // Mount MCP HTTP handler at /mcp
    let mcp_state = state.clone();
    let mcp_config = StreamableHttpServerConfig::default();
    let mcp_service = StreamableHttpService::new(
        move || {
            let handler = crate::mcp::ArminMcpHandler::new(mcp_state.clone());
            Ok::<_, std::io::Error>(handler)
        },
        Arc::new(NeverSessionManager::default()),
        mcp_config,
    );

    let app = Router::new()
        .route("/ws/stream", get(ws::ws_handler))
        .route("/query", post(routes::query_handler))
        .route("/graph", get(routes::graph_handler))
        .route("/debt", get(routes::debt_handler))
        .route("/communities", get(routes::community_handler))
        .route("/decisions", get(routes::decisions_handler))
        .route("/risks", get(routes::risks_handler))
        .route("/summary", get(routes::summary_handler))
        .route("/agent/decision", post(routes::agent_decision_handler))
        .route("/agent/question", post(routes::agent_question_handler))
        .route("/agent/resolve", post(routes::agent_resolve_handler))
        .route("/agent/invalidate", post(routes::agent_invalidate_handler))
        .route("/graph/diff", get(routes::diff_handler))
        .route("/ingest", post(routes::ingest_handler))
        .route("/mcp", any_service(mcp_service))
        .layer(middleware::from_fn_with_state(
            armin_http::optional_token(args.auth_token),
            armin_http::require_bearer,
        ))
        .layer(CorsLayer::permissive())
        .with_state(state);

    let addr = format!("{}:{}", args.bind, args.port);
    info!("ARMIN server listening on {addr} (MCP at /mcp)");
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}
