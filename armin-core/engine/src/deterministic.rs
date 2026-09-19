//! Tool-call classification (v2).
//!
//! In the durable-knowledge design, tool calls are never graph nodes. Read
//! tools become provenance only; mutating and verifying tool calls belong in
//! the per-session scratch layer (WP2), where they feed the cross-layer debt
//! detectors (`UnverifiedChange`, `FailedVerification`) and the scoped brief.
//! Until the scratch layer lands, tool events produce no nodes at all.

use armin_ingest::{EventKind, EventRecord};

/// Whether this tool-call event will carry graph value once the scratch
/// layer exists. Kept so the ingest path can still route on event kind.
pub fn is_tool_event(event: &EventRecord) -> bool {
    event.event_kind == EventKind::ToolCall
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool_event() -> EventRecord {
        EventRecord {
            id: "evt-1".into(),
            session_id: "s1".into(),
            agent_role: "agent".into(),
            start_time: 1.0,
            end_time: 2.0,
            text: "Edited src/main.rs successfully".into(),
            event_kind: EventKind::ToolCall,
            tool_name: Some("edit".into()),
            files: vec!["src/main.rs".into()],
            commit: None,
        }
    }

    #[test]
    fn recognizes_tool_events() {
        assert!(is_tool_event(&tool_event()));
    }
}
