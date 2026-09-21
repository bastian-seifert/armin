//! Engine-daemon lifecycle for Claude Code hook invocations.
//!
//! Hooks are short-lived processes, so the sidecar runs as a detached
//! per-project daemon. Each invocation locks a state file, probes the
//! recorded endpoint, and spawns a fresh engine (bundled binary, loopback
//! only, bearer-authenticated) when needed. The daemon exits itself via
//! `--idle-exit` when the project goes quiet; the next hook simply starts
//! another one. The state directory also carries per-session progress
//! (transcript cursor, recent files, last-injected brief hash).

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::util;

/// Trailing component distinguishing this harness's state from other harnesses.
const HARNESS: &str = "claude";
/// Handshake line the engine prints on stdout line 1.
const HANDSHAKE_PREFIX: &str = "ARMIN_PORT=";
/// How long to wait for the handshake before giving up.
const HANDSHAKE_TIMEOUT_MS: u64 = 5_000;
/// Per-request HTTP timeout for the engine API.
pub const HTTP_TIMEOUT: Duration = Duration::from_secs(5);
/// Health-probe timeout while deciding whether a daemon is alive.
const PROBE_TIMEOUT: Duration = Duration::from_millis(700);
/// armin-hook crate version — a change restarts the daemon so engine and
/// hook always ship as one version.
pub const HOOK_VERSION: &str = env!("CARGO_PKG_VERSION");

// ── State files ──────────────────────────────────────────────────────────────

/// `engine.json` — how to reach the running daemon.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct EngineState {
    pub port: u16,
    pub token: String,
    pub pid: u32,
    pub hook_version: String,
    pub started_at: u64,
}

/// `session-<sid>.json` — per-session progress.
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct SessionState {
    /// Byte offset into the transcript already ingested as assistant prose.
    #[serde(default)]
    pub cursor: u64,
    /// Recently touched files, most recent first (scopes the brief).
    #[serde(default)]
    pub recent_files: Vec<String>,
    /// Hash of the last brief injected into this session's context.
    #[serde(default)]
    pub brief_hash: String,
}

fn state_root() -> PathBuf {
    if let Ok(xdg) = std::env::var("XDG_STATE_HOME") {
        if !xdg.is_empty() {
            return PathBuf::from(xdg).join("armin").join(HARNESS);
        }
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join(".local/state/armin").join(HARNESS)
}

fn data_root() -> PathBuf {
    if let Ok(xdg) = std::env::var("XDG_DATA_HOME") {
        if !xdg.is_empty() {
            return PathBuf::from(xdg).join("armin").join(HARNESS);
        }
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join(".local/share/armin").join(HARNESS)
}

/// Project identity: git origin remote when available, else the working
/// directory. Sessions in any checkout of the same remote share one graph.
pub fn project_key(cwd: &Path) -> String {
    let output = std::process::Command::new("git")
        .args(["-C", &cwd.to_string_lossy(), "config", "--get", "remote.origin.url"])
        .output();
    if let Ok(out) = output {
        let url = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !url.is_empty() {
            return url.trim_end_matches(".git").to_string();
        }
    }
    cwd.to_string_lossy().to_string()
}

/// Directory holding this project's state files.
pub fn state_dir(cwd: &Path) -> PathBuf {
    let key = project_key(cwd);
    let slug = format!("{}-{}", util::slugify(&key), util::short_hash(&key));
    state_root().join(slug)
}

/// Sled database directory for the project graph.
fn db_file(cwd: &Path) -> PathBuf {
    let key = project_key(cwd);
    let db_dir = std::env::var("ARMIN_DB_DIR")
        .ok()
        .filter(|d| !d.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(data_root);
    db_dir.join(format!("{}.db", util::slugify(&key)))
}

// ── Locking ──────────────────────────────────────────────────────────────────

/// Cross-process mutex over the ensure path. On unix this is a flock; on
/// other platforms it degrades to a plain file handle (unsupported platforms
/// are rejected before the lock matters anyway).
struct FileLock {
    #[cfg(unix)]
    file: std::fs::File,
}

impl FileLock {
    #[allow(unused_mut)]
    fn acquire(path: &Path) -> Option<Self> {
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .read(true)
            .open(path)
            .ok()?;
        #[cfg(unix)]
        {
            use std::os::unix::io::AsRawFd;
            let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
            if rc != 0 {
                return None;
            }
            Some(FileLock { file })
        }
        #[cfg(not(unix))]
        {
            Some(FileLock {})
        }
    }
}

#[cfg(unix)]
impl Drop for FileLock {
    fn drop(&mut self) {
        use std::os::unix::io::AsRawFd;
        unsafe {
            libc::flock(self.file.as_raw_fd(), libc::LOCK_UN);
        }
    }
}

// ── Engine binary resolution ─────────────────────────────────────────────────

/// Platform tag matching the release artifact naming scheme.
pub fn platform_tag() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => Some("linux-x64"),
        ("macos", "x86_64") => Some("macos-x64"),
        ("macos", "aarch64") => Some("macos-arm64"),
        _ => None,
    }
}

/// Engine binary: ARMIN_ENGINE_BIN override, then a platform binary bundled
/// next to this hook, then a manual install in ~/.local/bin.
pub fn engine_bin() -> Option<PathBuf> {
    if let Ok(env) = std::env::var("ARMIN_ENGINE_BIN") {
        if !env.is_empty() {
            return Some(PathBuf::from(env));
        }
    }
    if let Some(self_path) = std::env::current_exe().ok().and_then(|p| p.canonicalize().ok()) {
        if let Some(parent) = self_path.parent() {
            if let Some(bin) = engine_bin_at(parent) {
                return Some(bin);
            }
        }
    }
    let home = std::env::var("HOME").ok()?;
    let fallback = PathBuf::from(home).join(".local/bin/armin-engine");
    fallback.exists().then_some(fallback)
}

/// Look for an engine binary in `dir` (platform-suffixed first, then bare).
pub fn engine_bin_at(dir: &Path) -> Option<PathBuf> {
    let tag = platform_tag()?;
    let names = [
        format!("armin-engine-{tag}"),
        format!("armin-engine-{tag}.exe"),
        "armin-engine".to_string(),
        "armin-engine.exe".to_string(),
    ];
    for name in names {
        let candidate = dir.join(&name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

// ── Environment (plugin options → sidecar env) ───────────────────────────────

/// Map the plugin's `userConfig` options (exposed as CLAUDE_PLUGIN_OPTION_*)
/// to the engine's environment variables. Explicit env vars always win.
fn sidecar_env_patch() -> Vec<(String, String)> {
    let mut patch = Vec::new();
    let mut push = |target: &str, source: &str| {
        if let Ok(val) = std::env::var(source) {
            if !val.is_empty() && std::env::var(target).unwrap_or_default().is_empty() {
                patch.push((target.to_string(), val));
            }
        }
    };
    push("TYPESAFE_AI_API_KEY", "CLAUDE_PLUGIN_OPTION_TYPESAFEKEY");
    push("OPENROUTER_API_KEY", "CLAUDE_PLUGIN_OPTION_OPENROUTERKEY");
    push("ARMIN_MODEL", "CLAUDE_PLUGIN_OPTION_MODEL");
    if std::env::var("CLAUDE_PLUGIN_OPTION_DEBUG").as_deref() == Ok("1") {
        patch.push(("ARMIN_DEBUG".to_string(), "1".to_string()));
    }
    patch
}

// ── Daemon handle + ensure ───────────────────────────────────────────────────

/// A reachable engine endpoint.
pub struct Daemon {
    pub port: u16,
    pub token: String,
}

impl Daemon {
    pub fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }
}

/// Read the engine state file, if present and parseable.
pub fn read_state(dir: &Path) -> Option<EngineState> {
    let text = std::fs::read_to_string(dir.join("engine.json")).ok()?;
    serde_json::from_str(&text).ok()
}

/// Read the per-session progress file (empty state when absent).
pub fn read_session(dir: &Path, session_id: &str) -> SessionState {
    let path = dir.join(format!("session-{session_id}.json"));
    std::fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

/// Persist the per-session progress file.
pub fn write_session(dir: &Path, session_id: &str, state: &SessionState) {
    let path = dir.join(format!("session-{session_id}.json"));
    if let Ok(json) = serde_json::to_string(state) {
        let _ = std::fs::write(path, json);
    }
}

/// Probe whether an engine answers at `port` with `token`.
pub fn probe(port: u16, token: &str) -> bool {
    let url = format!("http://127.0.0.1:{port}/api/v1/health");
    get_json(&url, token, PROBE_TIMEOUT).is_some()
}

/// Ensure a daemon for the project rooted at `cwd` is running. Returns the
/// endpoint, or None when ARMIN cannot run here (unsupported platform,
/// missing binary, failed spawn). Best-effort by design: hooks must never
/// fail the harness.
pub fn ensure(cwd: &Path, debug: bool) -> Option<Daemon> {
    let dir = state_dir(cwd);
    std::fs::create_dir_all(&dir).ok()?;
    let _lock = FileLock::acquire(&dir.join(".engine.lock"))?;
    logln(debug, &format!("ensure: lock held at {}", dir.display()));

    if let Some(state) = read_state(&dir) {
        if state.hook_version == HOOK_VERSION && probe(state.port, &state.token) {
            logln(debug, &format!("daemon alive on port {}", state.port));
            return Some(Daemon { port: state.port, token: state.token });
        }
        // Version changed (plugin update) or daemon died: retire the old
        // process if it still exists; a fresh engine takes over the graph.
        kill_pid(state.pid);
    }

    let pinned_failed = dir.join("pinned-failed").exists();
    let pinned_port = std::env::var("ARMIN_PORT")
        .ok()
        .and_then(|p| p.trim().parse::<u16>().ok())
        .filter(|p| *p > 0)
        .filter(|_| !pinned_failed);

    // A pinned port that failed once stays failed for this state dir —
    // later starts auto-assign (mirrors the opencode plugin).
    if let Some(pinned) = pinned_port {
        if let Some(daemon) = spawn_engine(cwd, &dir, Some(pinned), debug) {
            return Some(daemon);
        }
    }
    spawn_engine(cwd, &dir, None, debug)
}

/// Spawn the engine, wait for the port handshake, and record the state file.
fn spawn_engine(cwd: &Path, dir: &Path, pinned_port: Option<u16>, debug: bool) -> Option<Daemon> {
    let engine = engine_bin()?;
    std::fs::create_dir_all(dir).ok()?;
    let db_path = db_file(cwd);

    let token = crate::util::random_token();
    let mut cmd = std::process::Command::new(&engine);
    cmd.args([
        "--port".to_string(),
        pinned_port.map(|p| p.to_string()).unwrap_or_else(|| "0".into()),
        "--db-path".to_string(),
        db_path.to_string_lossy().into_owned(),
        "--auth-token".to_string(),
        token.clone(),
        "--idle-exit".to_string(),
        std::env::var("ARMIN_IDLE_EXIT").unwrap_or_else(|_| "1800".into()),
    ]);
    for (k, v) in sidecar_env_patch() {
        cmd.env(k, v);
    }
    configure_detached(&mut cmd, dir);

    let mut child = match cmd.stdout(std::process::Stdio::piped()).spawn() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("armin-hook: failed to spawn engine: {e}");
            return None;
        }
    };

    match read_handshake(&mut child) {
        Some(port) => {
            logln(debug, &format!("spawned engine pid {} on port {port}", child.id()));
            let state = EngineState {
                port,
                token,
                pid: child.id(),
                hook_version: HOOK_VERSION.into(),
                started_at: util::unix_secs(),
            };
            let daemon = Daemon { port, token: state.token.clone() };
            let _ = std::fs::write(
                dir.join("engine.json"),
                serde_json::to_string(&state).unwrap_or_default(),
            );
            Some(daemon)
        }
        None => {
            // Bind failure or no handshake. If this was a pinned-port
            // attempt, remember the failure so later starts auto-assign.
            logln(debug, "engine did not report ARMIN_PORT in time");
            let _ = child.kill();
            if pinned_port.is_some() {
                let _ = std::fs::write(dir.join("pinned-failed"), "1");
            }
            None
        }
    }
}

/// Detach the daemon from the hook process: its own process group (no signal
/// propagation), stderr into the state dir, stdin closed. The hook exits
/// right away; the engine is re-parented by init.
fn configure_detached(cmd: &mut std::process::Command, dir: &Path) {
    cmd.stdin(std::process::Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
        if let Ok(log) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join("engine.log"))
        {
            cmd.stderr(std::process::Stdio::from(log));
        } else {
            cmd.stderr(std::process::Stdio::null());
        }
    }
    #[cfg(not(unix))]
    {
        let _ = dir;
        cmd.stderr(std::process::Stdio::null());
    }
}

/// Read `ARMIN_PORT=<n>` from the child's stdout, with a hard timeout.
fn read_handshake(child: &mut std::process::Child) -> Option<u16> {
    let mut stdout = child.stdout.take()?;
    let (tx, rx) = mpsc::channel();
    let _reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let mut byte = [0u8; 1];
        loop {
            match stdout.read(&mut byte) {
                Ok(0) | Err(_) => {
                    let _ = tx.send(None);
                    return;
                }
                Ok(_) => {
                    buf.push(byte[0]);
                    if byte[0] == b'\n' {
                        let line = String::from_utf8_lossy(&buf);
                        if let Some(port) = line
                            .trim()
                            .strip_prefix(HANDSHAKE_PREFIX)
                            .and_then(|p| p.trim().parse::<u16>().ok())
                        {
                            let _ = tx.send(Some(port));
                            return;
                        }
                        buf.clear();
                    }
                }
            }
        }
    });
    let result = rx.recv_timeout(Duration::from_millis(HANDSHAKE_TIMEOUT_MS)).ok();
    // The engine never writes to stdout after the handshake, so the blocked
    // reader thread (if the handshake timed out) dies with this process.
    result?
}

fn kill_pid(pid: u32) {
    #[cfg(unix)]
    {
        if pid > 1 {
            unsafe {
                libc::kill(pid as i32, libc::SIGTERM);
            }
        }
    }
    #[cfg(not(unix))]
    let _ = pid;
}

// ── HTTP ─────────────────────────────────────────────────────────────────────

fn client(timeout: Duration) -> Option<reqwest::blocking::Client> {
    reqwest::blocking::Client::builder().timeout(timeout).build().ok()
}

/// GET a JSON document from the engine.
pub fn get_json(url: &str, token: &str, timeout: Duration) -> Option<serde_json::Value> {
    let client = client(timeout)?;
    let mut req = client.get(url);
    if !token.is_empty() {
        req = req.bearer_auth(token);
    }
    let value = req.send().ok()?.error_for_status().ok()?.json().ok()?;
    Some(value)
}

/// POST a JSON body to the engine.
pub fn post_json(
    url: &str,
    token: &str,
    body: &serde_json::Value,
    timeout: Duration,
) -> Option<serde_json::Value> {
    let client = client(timeout)?;
    let mut req = client.post(url).json(body);
    if !token.is_empty() {
        req = req.bearer_auth(token);
    }
    let value = req.send().ok()?.json().ok()?;
    Some(value)
}

// ── Logging ──────────────────────────────────────────────────────────────────

/// Verbose logging to stderr (goes to Claude Code's debug log, never the
/// transcript). Enabled by ARMIN_DEBUG=1 or the plugin's debug userConfig.
pub fn logln(debug: bool, msg: &str) {
    if debug || std::env::var("ARMIN_DEBUG").as_deref() == Ok("1") {
        eprintln!("[armin] {msg}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_key_prefers_git_origin() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        run_git(path, &["init", "-q"]);
        run_git(path, &["remote", "add", "origin", "https://github.com/a/b.git"]);
        let key = project_key(path);
        assert_eq!(key, "https://github.com/a/b");
    }

    #[test]
    fn project_key_falls_back_to_path() {
        let dir = tempfile::tempdir().unwrap();
        let key = project_key(dir.path());
        assert!(key.contains(&dir.path().to_string_lossy().as_ref()));
    }

    #[test]
    fn state_dir_is_stable_and_distinct() {
        let dir = tempfile::tempdir().unwrap();
        let a = state_dir(dir.path());
        let b = state_dir(dir.path());
        assert_eq!(a, b);
        let other = tempfile::tempdir().unwrap();
        assert_ne!(a, state_dir(other.path()));
    }

    #[test]
    fn platform_tag_is_recognized() {
        // On the platforms we build for, the tag must resolve; the test
        // binary runs on linux-x64 in CI, but keep the check generic.
        if cfg!(target_os = "linux") && cfg!(target_arch = "x86_64") {
            assert_eq!(platform_tag(), Some("linux-x64"));
        }
    }

    #[test]
    fn session_state_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let state = SessionState {
            cursor: 42,
            recent_files: vec!["/tmp/a.rs".into()],
            brief_hash: "abcd".into(),
        };
        write_session(dir.path(), "sess-1", &state);
        let back = read_session(dir.path(), "sess-1");
        assert_eq!(back.cursor, 42);
        assert_eq!(back.recent_files, vec!["/tmp/a.rs".to_string()]);
        assert_eq!(back.brief_hash, "abcd");
    }

    #[test]
    fn missing_session_state_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let state = read_session(dir.path(), "nope");
        assert_eq!(state.cursor, 0);
        assert!(state.recent_files.is_empty());
    }

    fn run_git(dir: &Path, args: &[&str]) {
        std::process::Command::new("git")
            .args(["-C", &dir.to_string_lossy()])
            .args(args)
            .output()
            .expect("git");
    }
}
