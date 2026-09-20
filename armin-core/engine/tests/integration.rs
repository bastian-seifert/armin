//! End-to-end integration test for the armin-engine sidecar.
//!
//! Spawns the real binary, drives the middleware flow a harness would use
//! (health → ingest → agent writes → brief → query → metrics), then restarts
//! against the same db path and verifies persistence.

use std::io::BufRead;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use serde_json::{Value, json};

#[allow(dead_code)]
fn now_ts() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs_f64()
}

struct Engine {
    child: Child,
    port: u16,
    token: String,
}

impl Drop for Engine {
    fn drop(&mut self) {
        // SIGTERM triggers the engine's graceful shutdown (sled flush).
        // SIGKILL would skip persistence, so never kill hard here.
        let _ = Command::new("kill")
            .args(["-TERM", &self.child.id().to_string()])
            .status();
        let _ = self.child.wait();
    }
}

fn find_engine_bin() -> Option<std::path::PathBuf> {
    // `cargo test` builds sibling bin targets and exposes them via this env var.
    std::env::var_os("CARGO_BIN_EXE_armin-engine").map(std::path::PathBuf::from)
}

fn start_engine(db_path: &std::path::Path, token: &str) -> Engine {
    let bin = find_engine_bin().expect("armin-engine binary not built; run `cargo build -p armin-engine`");
    let mut child = Command::new(&bin)
        .args([
            "--port",
            "0",
            "--db-path",
            db_path.to_str().unwrap(),
            "--auth-token",
            token,
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn armin-engine");

    let stdout = child.stdout.take().expect("stdout piped");
    let mut reader = std::io::BufReader::new(stdout);
    let mut line = String::new();
    reader.read_line(&mut line).expect("read handshake line");
    let port: u16 = line
        .trim()
        .strip_prefix("ARMIN_PORT=")
        .expect("ARMIN_PORT handshake")
        .parse()
        .expect("port number");

    Engine { child, port, token: token.to_string() }
}

fn url(port: u16, path: &str) -> String {
    format!("http://127.0.0.1:{port}/api/v1{path}")
}

fn get(engine: &Engine, path: &str) -> Value {
    let res = reqwest::blocking::Client::new()
        .get(url(engine.port, path))
        .header("Authorization", format!("Bearer {}", engine.token))
        .timeout(Duration::from_secs(10))
        .send()
        .expect("request");
    assert_eq!(res.status(), 200, "GET {path}");
    res.json().expect("json")
}

fn post(engine: &Engine, path: &str, body: Value) -> Value {
    let res = reqwest::blocking::Client::new()
        .post(url(engine.port, path))
        .header("Authorization", format!("Bearer {}", engine.token))
        .json(&body)
        .timeout(Duration::from_secs(10))
        .send()
        .expect("request");
    assert_eq!(res.status(), 200, "POST {path}");
    res.json().expect("json")
}

fn tool_event(id: &str, session: &str, text: &str, file: &str) -> Value {
    json!({
        "id": id,
        "session_id": session,
        "agent_role": "agent",
        "start_time": 1.0,
        "end_time": 2.0,
        "text": text,
        "event_kind": "tool_call",
        "tool_name": "edit",
        "files": [file],
        "commit": null,
    })
}

#[test]
fn engine_sidecar_end_to_end() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("graph.db");
    let token = "test-token-1234";

    // ── First run: capture, write, query ─────────────────────────────────
    {
        let engine = start_engine(&db_path, token);

        // Health.
        let health = get(&engine, "/health");
        assert_eq!(health["status"], "ok");
        assert_eq!(health["extraction"], "deterministic-only");

        // Auth is enforced.
        let anon = reqwest::blocking::Client::new()
            .get(url(engine.port, "/health"))
            .timeout(Duration::from_secs(5))
            .send()
            .unwrap();
        assert_eq!(anon.status(), 401);

        // Wrong token is rejected too.
        let wrong = reqwest::blocking::Client::new()
            .get(url(engine.port, "/health"))
            .header("Authorization", "Bearer nope")
            .timeout(Duration::from_secs(5))
            .send()
            .unwrap();
        assert_eq!(wrong.status(), 401);

        // Ingest: one tool event (not a graph node in v2 — scratch/provenance
        // comes in WP2) + one prose event (queued; worker exits immediately
        // without an API key — dropped).
        let resp = post(
            &engine,
            "/ingest",
            json!([
                tool_event("e1", "s1", "edit src/auth.rs: switched to JWT tokens", "src/auth.rs"),
                {
                    "id": "e2",
                    "session_id": "s1",
                    "agent_role": "assistant",
                    "start_time": 2.0,
                    "end_time": 3.0,
                    "text": "We should also consider rotating refresh tokens for the gateway.",
                    "event_kind": "utterance",
                }
            ]),
        );
        assert_eq!(resp["captured_deterministically"], 0);

        // Agent writes.
        let dec = post(
            &engine,
            "/agent/decision",
            json!({
                "label": "Use JWT for auth",
                "description": "Gateway supports it natively",
                "session_id": "s1",
                "files": ["src/auth.rs"],
            }),
        );
        assert!(!dec["node_id"].as_str().unwrap().is_empty());

        let q = post(
            &engine,
            "/agent/question",
            json!({
                "label": "Refresh-token rotation needed?",
                "description": "Security review pending",
                "session_id": "s1",
            }),
        );
        assert!(!q["node_id"].as_str().unwrap().is_empty());

        // Brief is non-empty now.
        let brief = get(&engine, "/state/brief");
        assert_eq!(brief["empty"], false);
        let brief_text = brief["brief"].as_str().unwrap();
        assert!(brief_text.contains("Decisions"));
        assert!(brief_text.contains("Open items"));

        // The tool event was an edit → scratch flags an unverified change.
        assert!(brief_text.contains("Unverified edits"), "brief: {brief_text}");

        // A passing test run over the same file clears the warning.
        let _resp = post(
            &engine,
            "/ingest",
            json!([{
                "id": "e3",
                "session_id": "s1",
                "agent_role": "agent",
                "start_time": 3.0,
                "end_time": 4.0,
                "text": "Bash cargo test: 47 passed, 0 failed",
                "event_kind": "tool_call",
                "tool_name": "bash",
                "files": ["src/auth.rs"],
            }]),
        );
        let brief = get(&engine, "/state/brief");
        let brief_text = brief["brief"].as_str().unwrap();
        assert!(
            !brief_text.contains("Unverified edits"),
            "check should clear the warning, brief: {brief_text}"
        );

        // Deterministic query finds the decision.
        let query = post(&engine, "/query", json!({ "question": "Why JWT auth?" }));
        assert!(!query["trace"].as_array().unwrap().is_empty());

        // Metrics reflect what happened. Tool events are counted (2: the
        // edit and the test run) but produce no nodes in v2; the two agent
        // writes are the only nodes.
        let metrics = get(&engine, "/metrics");
        assert_eq!(metrics["events_ingested"], 3);
        assert_eq!(metrics["tool_events"], 2);
        assert_eq!(metrics["deterministic_nodes"], 0);
        assert_eq!(metrics["agent_writes"], 2);
        assert_eq!(metrics["nodes_added"], 2);
    }

    // ── Restart: persistence across the restart boundary ─────────────────
    {
        let engine = start_engine(&db_path, token);

        let brief = get(&engine, "/state/brief");
        assert_eq!(brief["empty"], false, "graph must survive restart");
        let brief_text = brief["brief"].as_str().unwrap();
        assert!(brief_text.contains("Use JWT for auth"));

        let query = post(&engine, "/query", json!({ "question": "JWT auth" }));
        assert!(!query["trace"].as_array().unwrap().is_empty());
    }
}

#[test]
fn ingestion_is_idempotent_per_event() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("graph.db");
    let engine = start_engine(&db_path, "tok");

    let event = tool_event("fixed-id", "s1", "read file", "src/x.rs");
    post(&engine, "/ingest", json!([event.clone()]));
    post(&engine, "/ingest", json!([event]));

    let metrics = get(&engine, "/metrics");
    // v2: tool events are captured but produce no graph nodes (scratch
    // layer lands in WP2), so re-ingestion adds nothing.
    assert_eq!(metrics["nodes_added"], 0);
}
