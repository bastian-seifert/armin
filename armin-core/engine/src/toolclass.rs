//! Tool-call classification and the per-session scratch layer.
//!
//! In the durable-knowledge design, tool calls are never graph nodes. They
//! are classified — read, write (mutation), check (verification) — and land
//! in an in-memory per-session scratch: mutations and verification outcomes
//! feed the cross-layer debt detectors (`UnverifiedChange`,
//! `FailedVerification`, `RuleViolation`) and the brief's warnings section.
//! Scratch dies with the engine process; only durable knowledge persists.

use std::collections::HashMap;
use std::sync::Arc;

use armin_graph::{ScratchCheck, ScratchEdit, ScratchSnapshot};
use armin_ingest::EventRecord;
use tokio::sync::RwLock;

/// Maximum entries per scratch list per session (ring buffer).
const MAX_ENTRIES: usize = 100;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolClass {
    /// Observation only — provenance, no scratch entry.
    Read,
    /// Mutation — becomes a `ScratchEdit`.
    Write,
    /// Verification with an outcome — becomes a `ScratchCheck`.
    Check,
}

const WRITE_TOOLS: &[&str] = &[
    "edit",
    "write",
    "multiedit",
    "apply_patch",
    "applypatch",
    "patch",
    "create_file",
    "replace",
    "str_replace",
    "move",
    "remove",
    "rm",
    "delete",
    "notebook_edit",
    "insert_content",
    "search_and_replace",
];

const CHECK_TOOLS: &[&str] = &[
    "test",
    "run_tests",
    "lint",
    "typecheck",
    "type_check",
    "build",
    "compile",
    "diagnostics",
];

/// Shell command fragments that mark a bash/shell call as a verification.
const CHECK_COMMANDS: &[&str] = &[
    "cargo test",
    "cargo build",
    "cargo check",
    "cargo clippy",
    "npm test",
    "npm run",
    "npx tsc",
    "tsc ",
    "pytest",
    "python -m pytest",
    "go test",
    "go build",
    "go vet",
    "make test",
    "make check",
    "ruff",
    "eslint",
    "prettier --check",
    "bun test",
    "bunx tsc",
    "vitest",
    "jest",
];

/// Shell command fragments that mark a bash/shell call as a read-only
/// observation (anything that is neither clearly a check nor mutating).
const READ_COMMANDS: &[&str] = &[
    "ls", "cat ", "head ", "tail ", "grep", "rg ", "rg/", "find ", "wc ", "which ",
    "git status", "git log", "git diff", "git show", "git branch", "echo ", "pwd",
];

/// Classify a tool call by name and text.
pub fn classify(tool: &str, text: &str) -> ToolClass {
    let name = tool.to_ascii_lowercase();
    if WRITE_TOOLS.contains(&name.as_str()) {
        return ToolClass::Write;
    }
    if CHECK_TOOLS.contains(&name.as_str()) {
        return ToolClass::Check;
    }
    let lowered = text.to_ascii_lowercase();
    if name == "bash" || name == "shell" || name == "terminal" || name == "run_command" {
        if CHECK_COMMANDS.iter().any(|p| lowered.contains(p)) {
            return ToolClass::Check;
        }
        if READ_COMMANDS.iter().any(|p| lowered.contains(p)) {
            return ToolClass::Read;
        }
        // Unknown shell commands may mutate the world — treat as write so
        // unverified-change detection errs on the side of nagging.
        return ToolClass::Write;
    }
    ToolClass::Read
}

/// Heuristic outcome of a verification: `true` unless the output clearly
/// reports failure. Ambiguous output counts as passed — a check that ran
/// but whose result we cannot read still satisfies "something verified it".
pub fn check_passed(text: &str) -> bool {
    let lowered = text.to_ascii_lowercase();
    // Explicit all-clear wins first ("0 failed", "50 passed, 0 failed").
    let zero_failures = ["0 failed", "0 failures", "0 failing", "all tests passed",
        "no failures"];
    if zero_failures.iter().any(|p| lowered.contains(p)) {
        return true;
    }
    let hard_fail = ["failed", "failure", "error[", "error:", "assertionerror",
        "compilation error", "does not compile", "✗"];
    if hard_fail.iter().any(|p| lowered.contains(p)) {
        return false;
    }
    // Anything else counts as passed: a check that ran but whose result we
    // cannot read still satisfies "something verified it".
    true
}

/// Per-session in-memory scratch: recent mutations and verification
/// outcomes. Never persisted; dies with the engine process. Shared as
/// `Arc<Scratch>` (EngineState is Clone).
#[derive(Default)]
pub struct Scratch {
    sessions: RwLock<HashMap<String, SessionScratch>>,
}

#[derive(Default)]
struct SessionScratch {
    edits: Vec<ScratchEdit>,
    checks: Vec<ScratchCheck>,
}

impl Scratch {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub async fn record_edit(&self, session: &str, edit: ScratchEdit) {
        let mut sessions = self.sessions.write().await;
        let s = sessions.entry(session.to_string()).or_default();
        s.edits.push(edit);
        if s.edits.len() > MAX_ENTRIES {
            s.edits.remove(0);
        }
    }

    pub async fn record_check(&self, session: &str, check: ScratchCheck) {
        let mut sessions = self.sessions.write().await;
        let s = sessions.entry(session.to_string()).or_default();
        s.checks.push(check);
        if s.checks.len() > MAX_ENTRIES {
            s.checks.remove(0);
        }
    }

    /// Snapshot for the debt detectors (clone of the ring buffers).
    pub async fn snapshot(&self, session: &str) -> ScratchSnapshot {
        let sessions = self.sessions.read().await;
        match sessions.get(session) {
            Some(s) => ScratchSnapshot {
                edits: s.edits.clone(),
                checks: s.checks.clone(),
            },
            None => ScratchSnapshot::default(),
        }
    }
}

/// Record a ToolCall event into the scratch layer (no-op for reads).
pub async fn record_tool_event(scratch: &Scratch, event: &EventRecord) {
    let Some(tool) = event.tool_name.as_deref() else { return };
    match classify(tool, &event.text) {
        ToolClass::Read => {}
        ToolClass::Write => {
            scratch
                .record_edit(
                    &event.session_id,
                    ScratchEdit {
                        event_id: event.id.clone(),
                        tool: tool.to_string(),
                        files: event.files.clone(),
                        timestamp: event.end_time,
                    },
                )
                .await;
        }
        ToolClass::Check => {
            scratch
                .record_check(
                    &event.session_id,
                    ScratchCheck {
                        event_id: event.id.clone(),
                        tool: tool.to_string(),
                        files: event.files.clone(),
                        passed: check_passed(&event.text),
                        timestamp: event.end_time,
                    },
                )
                .await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use armin_ingest::EventKind;

    fn tool_event(tool: &str, text: &str) -> EventRecord {
        EventRecord {
            id: format!("ev-{tool}-{}", text.len()),
            session_id: "s1".into(),
            agent_role: "agent".into(),
            start_time: 1.0,
            end_time: 2.0,
            text: text.into(),
            event_kind: EventKind::ToolCall,
            tool_name: Some(tool.into()),
            files: vec!["src/main.rs".into()],
            commit: None,
        }
    }

    #[test]
    fn classification_table() {
        assert_eq!(classify("edit", "x"), ToolClass::Write);
        assert_eq!(classify("write", "x"), ToolClass::Write);
        assert_eq!(classify("read", "x"), ToolClass::Read);
        assert_eq!(classify("grep", "x"), ToolClass::Read);
        assert_eq!(classify("test", "x"), ToolClass::Check);
        assert_eq!(classify("lint", "x"), ToolClass::Check);
        assert_eq!(classify("bash", "Bash cargo test: all passed"), ToolClass::Check);
        assert_eq!(classify("bash", "Bash ls -la: files"), ToolClass::Read);
        assert_eq!(classify("bash", "Bash rm -rf build: done"), ToolClass::Write);
    }

    #[test]
    fn outcomes() {
        assert!(check_passed("42 passed, 0 failed"));
        assert!(!check_passed("2 failed | 40 passed"));
        assert!(!check_passed("error[E0308]: mismatched types"));
        assert!(check_passed("Compilation finished")); // ambiguous → pass
    }

    #[tokio::test]
    async fn scratch_records_and_snapshots() {
        let scratch = Scratch::new();
        record_tool_event(&scratch, &tool_event("edit", "edited file")).await;
        record_tool_event(&scratch, &tool_event("bash", "Bash cargo test: 5 passed")).await;
        let snap = scratch.snapshot("s1").await;
        assert_eq!(snap.edits.len(), 1);
        assert_eq!(snap.checks.len(), 1);
        assert!(snap.checks[0].passed);
    }
}
