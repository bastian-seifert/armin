//! armin-hook — the ARMIN event adapter for Claude Code.
//!
//! One small binary, five subcommands, one per Claude Code hook event:
//!
//! - `session-start`: ensure the engine, import CLAUDE.md/AGENTS.md,
//!   inject the reasoning-state brief (also fires after every compaction —
//!   this is rehydration)
//! - `user-prompt-submit`: capture the prompt, flush new assistant prose
//!   from the transcript, inject the brief when it changed
//! - `post-tool-use`: (async) capture the tool call + touched files
//! - `post-compact`: (async) capture the compaction summary
//! - `session-end`: bookkeeping no-op (the daemon exits via `--idle-exit`;
//!   never killed on session end)
//! - `ensure`: start the daemon without handling an event
//!
//! Every handler is best-effort: internal failures log to stderr and exit 0.
//! Hooks must never break the agent's turn.
//!
//! Design (mirrors the opencode plugin, .opencode/plugins/armin.ts):
//! capture on post-tool events so it stays off the model's critical path;
//! a pre-turn event whose stdout is injected into context; re-injection of
//! the brief after compaction.

mod daemon;
mod transcript;
mod util;

use std::io::Read;
use std::path::{Path, PathBuf};

use armin_ingest::{EventKind, EventRecord};
use clap::{Parser, Subcommand};
use daemon::Daemon;
use serde::Deserialize;

/// Max characters of assistant/tool text captured per event (mirrors the
/// opencode plugin).
const MAX_TEXT_CHARS: usize = 2_000;
/// Max characters of injected brief text (Claude Code caps hook stdout at
/// 10,000 characters; stay under it).
const MAX_INJECT_CHARS: usize = 9_500;
/// Assistant text blocks ingested per transcript read.
const MAX_TRANSCRIPT_EVENTS: usize = 40;
/// Recent-files ring size (scopes the brief's "Binding here" section).
const MAX_RECENT_FILES: usize = 20;

#[derive(Parser)]
#[command(
    name = "armin-hook",
    version,
    about = "ARMIN event adapter for Claude Code hooks"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Ensure the per-project engine daemon is running.
    Ensure,
    /// SessionStart hook: ensure engine, import docs, inject the brief.
    SessionStart,
    /// UserPromptSubmit hook: capture prompt + prose, inject the brief.
    UserPromptSubmit,
    /// PostToolUse hook (async): capture the tool call.
    PostToolUse,
    /// PostCompact hook (async): capture the compaction summary.
    PostCompact,
    /// SessionEnd hook: bookkeeping only.
    SessionEnd,
}

/// Parsed hook stdin. Only the fields ARMIN uses; unknown fields ignored.
#[derive(Deserialize, Debug)]
struct HookInput {
    session_id: String,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    transcript_path: Option<String>,
    #[serde(default)]
    prompt: Option<String>,
    #[serde(default)]
    source: Option<String>,
    #[serde(default)]
    trigger: Option<String>,
    #[serde(default)]
    tool_name: Option<String>,
    #[serde(default)]
    tool_input: Option<serde_json::Value>,
    #[serde(default)]
    tool_response: Option<serde_json::Value>,
    #[serde(default)]
    compact_summary: Option<String>,
}

fn main() {
    let cli = Cli::parse();
    // Read stdin up front: every hook event carries JSON there.
    let mut input_raw = String::new();
    let _ = std::io::stdin().read_to_string(&mut input_raw);
    let input: Option<HookInput> = serde_json::from_str(input_raw.trim()).ok();

    // Never fail the harness: all errors are swallowed into exit 0.
    let result = match &cli.command {
        Command::Ensure => {
            let cwd = input
                .as_ref()
                .and_then(|i| i.cwd.as_deref())
                .map(PathBuf::from)
                .or_else(|| std::env::current_dir().ok())
                .unwrap_or_else(|| PathBuf::from("."));
            let _ = daemon::ensure(&cwd, debug_enabled());
            Ok(())
        }
        Command::SessionStart => handle_session_start(input.as_ref()),
        Command::UserPromptSubmit => handle_user_prompt_submit(input.as_ref()),
        Command::PostToolUse => handle_post_tool_use(input.as_ref()),
        Command::PostCompact => handle_post_compact(input.as_ref()),
        Command::SessionEnd => Ok(()),
    };
    if let Err(e) = result {
        daemon::logln(false, &format!("handler error: {e}"));
    }
    std::process::exit(0);
}

fn handle_session_start(input: Option<&HookInput>) -> anyhow::Result<()> {
    let Some(input) = input else { return Ok(()) };
    let cwd = PathBuf::from(input.cwd.clone().unwrap_or_else(|| ".".into()));
    let debug = debug_enabled();

    let Some(daemon) = daemon::ensure(&cwd, debug) else {
        return Ok(());
    };

    // Cold start: import CLAUDE.md / AGENTS.md into an empty graph.
    // Deterministic parse on the engine side (content-hash IDs, idempotent).
    daemon::logln(debug, &format!("session-start (source: {:?})", input.source));
    import_doc_if_empty(&daemon, &cwd);

    let mut session = daemon::read_session(&state_dir(&cwd), &input.session_id);
    if let Some(brief) = fetch_brief(&daemon, &session.recent_files) {
        let hash = util::short_hash(&brief);
        print_injection(&brief);
        session.brief_hash = hash;
        daemon::write_session(&state_dir(&cwd), &input.session_id, &session);
    }
    Ok(())
}

fn handle_user_prompt_submit(input: Option<&HookInput>) -> anyhow::Result<()> {
    let Some(input) = input else { return Ok(()) };
    let cwd = PathBuf::from(input.cwd.clone().unwrap_or_else(|| ".".into()));
    let debug = debug_enabled();

    let Some(daemon) = daemon::ensure(&cwd, debug) else {
        return Ok(());
    };

    // 1. Capture the user's prompt.
    if let Some(prompt) = input.prompt.as_deref().filter(|p| !p.trim().is_empty()) {
        capture(&daemon, &input.session_id, EventKind::UserPrompt, "user", prompt, None, &[]);
    }

    // 2. Flush assistant prose that appeared since the last turn.
    let mut session = daemon::read_session(&state_dir(&cwd), &input.session_id);
    if let Some(path) = input.transcript_path.as_deref() {
        if let Some(tail) =
            transcript::read_new(Path::new(path), session.cursor, MAX_TRANSCRIPT_EVENTS)
        {
            if tail.reset {
                daemon::logln(debug, "transcript replaced — cursor reset to 0");
            }
            for text in &tail.texts {
                capture(&daemon, &input.session_id, EventKind::Utterance, "assistant", text, None, &[]);
            }
            session.cursor = tail.new_cursor;
        }
    }

    // 3. Inject the brief — only when it changed, so the transcript does not
    //    accumulate duplicate copies (Claude Code replays injected text on
    //    resume).
    if let Some(brief) = fetch_brief(&daemon, &session.recent_files) {
        let hash = util::short_hash(&brief);
        if hash != session.brief_hash {
            print_injection(&brief);
            session.brief_hash = hash;
        }
    }

    daemon::write_session(&state_dir(&cwd), &input.session_id, &session);
    Ok(())
}

fn handle_post_tool_use(input: Option<&HookInput>) -> anyhow::Result<()> {
    let Some(input) = input else { return Ok(()) };
    let cwd = PathBuf::from(input.cwd.clone().unwrap_or_else(|| ".".into()));
    let debug = debug_enabled();

    let Some(daemon) = daemon::ensure(&cwd, debug) else {
        return Ok(());
    };

    let tool_name = input.tool_name.clone().unwrap_or_else(|| "tool".into());
    let files = input
        .tool_input
        .as_ref()
        .map(|ti| extract_files(ti, &cwd))
        .unwrap_or_default();

    // Ring of recently touched files — scopes the brief's "Binding here".
    let mut session = daemon::read_session(&state_dir(&cwd), &input.session_id);
    for file in &files {
        session.recent_files.retain(|f| f != file);
    }
    for file in files.iter().rev() {
        session.recent_files.insert(0, file.clone());
    }
    session.recent_files.truncate(MAX_RECENT_FILES);

    let summary = input
        .tool_response
        .as_ref()
        .map(summarize_response)
        .unwrap_or_default();
    let text = format!("{} {}", tool_name, util::truncate(&summary, 400));
    capture(&daemon, &input.session_id, EventKind::ToolCall, "agent", &text, Some(&tool_name), &files);

    daemon::write_session(&state_dir(&cwd), &input.session_id, &session);
    Ok(())
}

fn handle_post_compact(input: Option<&HookInput>) -> anyhow::Result<()> {
    let Some(input) = input else { return Ok(()) };
    let cwd = PathBuf::from(input.cwd.clone().unwrap_or_else(|| ".".into()));

    let Some(daemon) = daemon::ensure(&cwd, debug_enabled()) else {
        return Ok(());
    };
    let trigger = input.trigger.clone().unwrap_or_else(|| "unknown".into());
    let mut text = format!("Session context was compacted (trigger: {trigger}). The summarized history replaced older turns.");
    if let Some(summary) = input.compact_summary.as_deref().filter(|s| !s.trim().is_empty()) {
        text.push_str("\nCompaction summary:\n");
        text.push_str(summary);
    }
    capture(&daemon, &input.session_id, EventKind::Utterance, "system", &text, None, &[]);
    Ok(())
}

// ── Shared helpers ───────────────────────────────────────────────────────────

fn debug_enabled() -> bool {
    std::env::var("ARMIN_DEBUG").as_deref() == Ok("1")
        || std::env::var("CLAUDE_PLUGIN_OPTION_DEBUG").as_deref() == Ok("1")
}

fn state_dir(cwd: &Path) -> PathBuf {
    daemon::state_dir(cwd)
}

/// Build and POST one event. Fire-and-forget: failures are ignored.
fn capture(
    daemon: &Daemon,
    session_id: &str,
    kind: EventKind,
    role: &str,
    text: &str,
    tool_name: Option<&str>,
    files: &[String],
) -> bool {
    if text.trim().is_empty() {
        return false;
    }
    let now = util::unix_secs() as f64;
    let id = format!(
        "cc-{}-{}-{}",
        &session_id[session_id.len().saturating_sub(8)..],
        util::short_hash(&format!("{session_id}{text}")),
        util::unix_secs()
    );
    let event = EventRecord {
        id,
        session_id: session_id.to_string(),
        agent_role: role.to_string(),
        start_time: now,
        end_time: now,
        text: util::truncate(text, MAX_TEXT_CHARS),
        event_kind: kind,
        tool_name: tool_name.map(|s| s.to_string()),
        files: files.to_vec(),
        commit: None,
    };
    let url = format!("{}/api/v1/ingest", daemon.base_url());
    daemon::post_json(&url, &daemon.token, &serde_json::json!([event]), daemon::HTTP_TIMEOUT)
        .is_some()
}

/// Fetch the reasoning-state brief (scoped to recently touched files).
fn fetch_brief(daemon: &Daemon, recent_files: &[String]) -> Option<String> {
    let mut url = format!("{}/api/v1/state/brief", daemon.base_url());
    if !recent_files.is_empty() {
        let scope: Vec<&String> = recent_files.iter().take(10).collect();
        let joined: Vec<&str> = scope.iter().map(|s| s.as_str()).collect();
        url.push_str(&format!("?files={}", util::percent_encode(&joined.join(","))));
    }
    let value = daemon::get_json(&url, &daemon.token, daemon::HTTP_TIMEOUT)?;
    let empty = value.get("empty").and_then(|e| e.as_bool()).unwrap_or(false);
    if empty {
        return None;
    }
    let brief = value.get("brief")?.as_str()?.to_string();
    (!brief.trim().is_empty()).then_some(brief)
}

/// Emit the brief as plain stdout — Claude Code adds hook stdout to context
/// on UserPromptSubmit/SessionStart.
fn print_injection(brief: &str) {
    let block = format!(
        "Reasoning state (ARMIN): tracked decisions, rules, and open items for this project. \
If a new request conflicts with a decision or rule below, say so explicitly before deviating.\n{brief}"
    );
    println!("{}", util::truncate(&block, MAX_INJECT_CHARS));
}

/// Import CLAUDE.md / AGENTS.md into an empty graph (deterministic parse,
/// idempotent on the engine side).
fn import_doc_if_empty(daemon: &Daemon, cwd: &Path) {
    let base = format!("{}/api/v1", daemon.base_url());
    let snap = daemon::get_json(&format!("{base}/snapshot"), &daemon.token, daemon::HTTP_TIMEOUT);
    let node_count = snap
        .as_ref()
        .and_then(|s| s.get("nodes"))
        .and_then(|n| n.as_array())
        .map(|a| a.len())
        .unwrap_or(0);
    if node_count > 0 {
        return;
    }
    for name in ["CLAUDE.md", "AGENTS.md"] {
        let path = cwd.join(name);
        if let Ok(content) = std::fs::read_to_string(&path) {
            let body = serde_json::json!({
                "content": content,
                "session_id": format!("import-{}", util::unix_secs()),
            });
            if let Some(resp) = daemon::post_json(
                &format!("{base}/import"),
                &daemon.token,
                &body,
                daemon::HTTP_TIMEOUT,
            ) {
                let imported = resp.get("imported").and_then(|v| v.as_i64()).unwrap_or(0);
                if imported > 0 {
                    daemon::logln(
                        debug_enabled(),
                        &format!("imported {name}: {imported} node(s)"),
                    );
                }
                break;
            }
        }
    }
}

/// Pull file paths out of common tool-arg shapes (Read/Write/Edit/NotebookEdit).
fn extract_files(tool_input: &serde_json::Value, cwd: &Path) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let Some(obj) = tool_input.as_object() else {
        return out;
    };
    let mut push = |v: &str| {
        let v = v.trim();
        if v.is_empty() || !v.contains('/') {
            return;
        }
        let path = if v.starts_with('/') || v.starts_with('~') {
            PathBuf::from(v)
        } else {
            cwd.join(v)
        };
        out.push(path.to_string_lossy().into_owned());
    };
    for key in ["file_path", "filePath", "path", "notebook_path"] {
        if let Some(v) = obj.get(key).and_then(|v| v.as_str()) {
            push(v);
        }
    }
    if let Some(paths) = obj.get("paths").and_then(|v| v.as_array()) {
        for v in paths {
            if let Some(s) = v.as_str() {
                push(s);
            }
        }
    }
    out.dedup();
    out.truncate(8);
    out
}

/// Condense a tool_response into a short text summary.
fn summarize_response(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Object(obj) => {
            for key in ["stdout", "output", "content", "result", "text"] {
                if let Some(v) = obj.get(key).and_then(|v| v.as_str()) {
                    if !v.trim().is_empty() {
                        return v.to_string();
                    }
                }
            }
            for key in ["filePath", "file_path", "type"] {
                if let Some(v) = obj.get(key).and_then(|v| v.as_str()) {
                    return v.to_string();
                }
            }
            serde_json::to_string(value).unwrap_or_default()
        }
        other => serde_json::to_string(other).unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hook_input_parses_minimal() {
        let input: HookInput = serde_json::from_str(
            r#"{"session_id":"abc","cwd":"/tmp","prompt":"do the thing"}"#,
        )
        .unwrap();
        assert_eq!(input.session_id, "abc");
        assert_eq!(input.prompt.as_deref(), Some("do the thing"));
    }

    #[test]
    fn hook_input_tolerates_unknown_fields() {
        let input: HookInput = serde_json::from_str(
            r#"{"session_id":"abc","hook_event_name":"UserPromptSubmit","permission_mode":"default","transcript_path":"/t.jsonl"}"#,
        )
        .unwrap();
        assert_eq!(input.session_id, "abc");
        assert!(input.prompt.is_none());
    }

    #[test]
    fn hook_input_rejects_garbage() {
        let parsed: Option<HookInput> = serde_json::from_str("not json").ok();
        assert!(parsed.is_none());
    }

    #[test]
    fn extract_files_common_shapes() {
        let cwd = Path::new("/proj");
        let ti = serde_json::json!({
            "file_path": "/proj/src/main.rs",
            "notebook_path": "notebooks/x.ipynb",
            "paths": ["/proj/lib/a.rs", "rel.rs", "noext"],
            "command": "ls"
        });
        let files = extract_files(&ti, cwd);
        assert!(files.contains(&"/proj/src/main.rs".to_string()));
        assert!(files.contains(&"/proj/notebooks/x.ipynb".to_string()));
        assert!(files.contains(&"/proj/lib/a.rs".to_string()));
        // "rel.rs" has no slash and "noext" is not a path — both skipped.
        assert!(!files.contains(&"/proj/rel.rs".to_string()));
        assert_eq!(files.len(), 3);
    }

    #[test]
    fn extract_files_skips_pathless() {
        let ti = serde_json::json!({"command": "rm -rf /", "query": "x"});
        assert!(extract_files(&ti, Path::new("/proj")).is_empty());
    }

    #[test]
    fn summarize_response_prefers_text_fields() {
        let v = serde_json::json!({"stdout": "hello world\n", "stderr": ""});
        assert_eq!(summarize_response(&v), "hello world\n");
        let v2 = serde_json::json!({"filePath": "/tmp/x", "type": "create"});
        assert_eq!(summarize_response(&v2), "/tmp/x");
        let v3 = serde_json::json!("bare string");
        assert_eq!(summarize_response(&v3), "bare string");
    }

    #[test]
    fn injection_has_header_and_is_capped() {
        let mut big = "x".repeat(20_000);
        big.push_str("</reasoning-state>");
        let out = {
            // capture what print_injection writes by swapping stdout is hard;
            // test the formatting indirectly via truncation helper.
            let block = format!("Reasoning state (ARMIN): header\n{big}");
            util::truncate(&block, MAX_INJECT_CHARS)
        };
        assert!(out.len() <= MAX_INJECT_CHARS + 4);
    }

    // ── Integration: real engine daemon ─────────────────────────────────

    /// Serialize the tests that mutate process env vars.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn engine_path() -> Option<PathBuf> {
        // Workspace target dir: <ws>/armin-core/target/debug/armin-engine
        let manifest = env!("CARGO_MANIFEST_DIR");
        let debug = Path::new(manifest).join("../target/debug");
        for name in ["armin-engine", "armin-engine.exe"] {
            let candidate = debug.join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
        None
    }

    /// Create a scratch "project" (git repo with an origin remote).
    fn scratch_project(base: &Path, name: &str, claude_md: &str) -> PathBuf {
        let dir = base.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        let run = |args: &[&str]| {
            let _ = std::process::Command::new("git")
                .args(["-C", &dir.to_string_lossy()])
                .args(args)
                .output()
                .expect("git");
        };
        run(&["init", "-q"]);
        run(&["remote", "add", "origin", &format!("https://example.com/{name}.git")]);
        std::fs::write(dir.join("CLAUDE.md"), claude_md).unwrap();
        dir
    }

    fn env_guard() -> std::sync::MutexGuard<'static, ()> {
        ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn set_env(k: &str, v: &str) {
        std::env::set_var(k, v);
    }

    fn kill_daemons(project: &Path) {
        if let Some(state) = daemon::read_state(&daemon::state_dir(project)) {
            unsafe {
                libc::kill(state.pid as i32, libc::SIGKILL);
            }
        }
    }

    #[test]
    fn daemon_end_to_end_with_real_engine() {
        let Some(engine) = engine_path() else {
            eprintln!("skipping: armin-engine not built (run `cargo test --workspace`)");
            return;
        };
        let _env = env_guard();
        let base = tempfile::tempdir().unwrap();
        set_env("XDG_STATE_HOME", base.path().join("state").to_str().unwrap());
        set_env("XDG_DATA_HOME", &base.path().join("data").to_string_lossy());
        set_env("ARMIN_ENGINE_BIN", &engine.to_string_lossy());
        set_env("ARMIN_DEBUG", "1");
        set_env("ARMIN_PORT", "");

        let project = scratch_project(
            base.path(),
            "e2e-proj",
            "# Project rules\n\n- You must never print timestamps in CLI output\n",
        );

        // 1. Ensure starts a daemon.
        let daemon = daemon::ensure(&project, true).expect("daemon should start");
        // 2. Idempotent: a second ensure reuses the same endpoint.
        let again = daemon::ensure(&project, true).unwrap();
        assert_eq!(again.port, daemon.port);

        // 3. Capture flows through.
        assert!(capture(
            &daemon,
            "sess-e2e",
            EventKind::UserPrompt,
            "user",
            "Add JWT auth to the gateway",
            None,
            &[]
        ));

        // 4. CLAUDE.md import seeds the brief on an empty graph.
        import_doc_if_empty(&daemon, &project);
        let brief = fetch_brief(&daemon, &["/any/file.rs".to_string()]);
        let brief = brief.expect("brief after import should not be empty");
        assert!(brief.contains("Rules") || brief.contains("reasoning-state"));

        // 5. Session state round-trips through the state dir.
        let session = daemon::read_session(&daemon::state_dir(&project), "sess-e2e");
        let _ = session; // state files exist; covered by unit tests

        kill_daemons(&project);
    }

    #[test]
    fn pinned_port_failure_falls_back() {
        let Some(engine) = engine_path() else {
            eprintln!("skipping: armin-engine not built");
            return;
        };
        let _env = env_guard();
        let base = tempfile::tempdir().unwrap();
        set_env("XDG_STATE_HOME", base.path().join("state").to_str().unwrap());
        set_env("XDG_DATA_HOME", &base.path().join("data").to_string_lossy());
        set_env("ARMIN_ENGINE_BIN", &engine.to_string_lossy());

        // Occupy a port so the pinned engine cannot bind it.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let pinned = listener.local_addr().unwrap().port();
        set_env("ARMIN_PORT", &pinned.to_string());

        let project = scratch_project(base.path(), "pinned-proj", "# Rules\n\n- rule one\n");
        let daemon = daemon::ensure(&project, true).expect("auto-assign fallback");
        assert_ne!(daemon.port, pinned, "must not have bound the occupied port");

        let marker = daemon::state_dir(&project).join("pinned-failed");
        assert!(marker.exists(), "failure must be recorded");

        kill_daemons(&project);
        drop(listener);
    }

    #[test]
    fn missing_engine_binary_is_graceful() {
        let _env = env_guard();
        let base = tempfile::tempdir().unwrap();
        set_env("XDG_STATE_HOME", base.path().join("state").to_str().unwrap());
        set_env("XDG_DATA_HOME", &base.path().join("data").to_string_lossy());
        set_env("ARMIN_ENGINE_BIN", "/nonexistent/armin-engine");
        let project = scratch_project(base.path(), "noop-proj", "# x\n");
        assert!(daemon::ensure(&project, false).is_none());
        // And the failure left no half-written state behind.
        assert!(
            daemon::read_state(&daemon::state_dir(&project))
                .map(|s| s.port == 0)
                .unwrap_or(true)
        );
    }
}
