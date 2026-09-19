use std::path::PathBuf;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// What kind of activity produced an event.
///
/// Drives the extraction strategy: `ToolCall` events are converted to
/// Evidence nodes deterministically (no LLM), prose events are queued for
/// LLM batch extraction.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Hash, Default)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    /// Free-form prose from an agent or human participant.
    #[default]
    Utterance,
    /// A tool invocation (deterministic Evidence extraction).
    ToolCall,
    /// The initial prompt from the user.
    UserPrompt,
}

/// A single event from a session — analogous to an utterance or tool call.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EventRecord {
    pub id: String,
    pub session_id: String,
    pub agent_role: String,
    pub start_time: f64,
    pub end_time: f64,
    pub text: String,
    /// What produced this event (defaults to Utterance for legacy inputs).
    #[serde(default)]
    pub event_kind: EventKind,
    /// Tool name for ToolCall events (e.g. "edit", "bash").
    #[serde(default)]
    pub tool_name: Option<String>,
    /// File paths referenced by this event.
    #[serde(default)]
    pub files: Vec<String>,
    /// Commit hash referenced by this event, if any.
    #[serde(default)]
    pub commit: Option<String>,
}

impl EventRecord {
    /// Approximate argumentative-signal prefilter: short procedural text
    /// (confirmations, greetings, empty tool results) is unlikely to yield
    /// argument structure, so we skip the LLM call for it.
    pub fn has_extraction_signal(&self, min_words: usize) -> bool {
        self.text.split_whitespace().count() >= min_words
    }
}

/// Something that produces a stream of events.
#[async_trait]
pub trait EventSource: Send {
    /// Return the next event, or `None` when the stream is exhausted.
    async fn next_event(&mut self) -> anyhow::Result<Option<EventRecord>>;
}

/// Reads events from a JSONL file, one JSON object per line.
pub struct JsonlSource {
    reader: Box<dyn tokio::io::AsyncBufRead + Send + Unpin>,
    _path: PathBuf,
}

impl JsonlSource {
    /// Open a JSONL file at the given path.
    pub async fn from_path(path: PathBuf) -> anyhow::Result<Self> {
        let file = tokio::fs::File::open(&path).await?;
        let reader = Box::new(tokio::io::BufReader::new(file));
        Ok(Self { reader, _path: path })
    }

    /// Wrap an already-open buffered reader.
    pub fn from_reader(reader: Box<dyn tokio::io::AsyncBufRead + Send + Unpin>, name: PathBuf) -> Self {
        Self { reader, _path: name }
    }
}

#[async_trait]
impl EventSource for JsonlSource {
    async fn next_event(&mut self) -> anyhow::Result<Option<EventRecord>> {
        use tokio::io::AsyncBufReadExt;

        let mut line = String::new();
        loop {
            line.clear();
            let bytes = self.reader.read_line(&mut line).await?;
            if bytes == 0 {
                return Ok(None);
            }
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            match serde_json::from_str::<EventRecord>(trimmed) {
                Ok(record) => return Ok(Some(record)),
                Err(e) => {
                    tracing::warn!("Skipping malformed line: {e}");
                    continue;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[tokio::test]
    async fn test_jsonl_source_reads_events() {
        let dir = std::env::temp_dir();
        let path = dir.join("test_ingest.jsonl");
        let mut file = std::fs::File::create(&path).unwrap();
        writeln!(file, r#"{{"id":"e1","session_id":"s1","agent_role":"PM","start_time":0.0,"end_time":1.0,"text":"hello"}}"#).unwrap();
        writeln!(file, r#"{{"id":"e2","session_id":"s1","agent_role":"ME","start_time":1.0,"end_time":2.0,"text":"world"}}"#).unwrap();
        file.flush().unwrap();

        let mut source = JsonlSource::from_path(path.clone()).await.unwrap();
        let e1 = source.next_event().await.unwrap().unwrap();
        assert_eq!(e1.id, "e1");
        assert_eq!(e1.text, "hello");
        let e2 = source.next_event().await.unwrap().unwrap();
        assert_eq!(e2.id, "e2");
        assert_eq!(e2.text, "world");
        assert!(source.next_event().await.unwrap().is_none());

        std::fs::remove_file(&path).ok();
    }

    #[tokio::test]
    async fn test_jsonl_source_skips_empty_lines() {
        let dir = std::env::temp_dir();
        let path = dir.join("test_ingest_skip.jsonl");
        let mut file = std::fs::File::create(&path).unwrap();
        writeln!(file, r#"{{"id":"e1","session_id":"s1","agent_role":"PM","start_time":0.0,"end_time":1.0,"text":"a"}}"#).unwrap();
        writeln!(file, "").unwrap();
        writeln!(file, r#"{{"id":"e2","session_id":"s1","agent_role":"ME","start_time":1.0,"end_time":2.0,"text":"b"}}"#).unwrap();
        file.flush().unwrap();

        let mut source = JsonlSource::from_path(path.clone()).await.unwrap();
        let e1 = source.next_event().await.unwrap().unwrap();
        assert_eq!(e1.id, "e1");
        let e2 = source.next_event().await.unwrap().unwrap();
        assert_eq!(e2.id, "e2");
        assert!(source.next_event().await.unwrap().is_none());

        std::fs::remove_file(&path).ok();
    }

    #[tokio::test]
    async fn test_jsonl_source_skips_malformed_lines() {
        let dir = std::env::temp_dir();
        let path = dir.join("test_ingest_malformed.jsonl");
        let mut file = std::fs::File::create(&path).unwrap();
        writeln!(file, r#"{{"id":"e1","session_id":"s1","agent_role":"PM","start_time":0.0,"end_time":1.0,"text":"ok"}}"#).unwrap();
        writeln!(file, "not json").unwrap();
        writeln!(file, r#"{{"id":"e2","session_id":"s1","agent_role":"ME","start_time":1.0,"end_time":2.0,"text":"also ok"}}"#).unwrap();
        file.flush().unwrap();

        let mut source = JsonlSource::from_path(path.clone()).await.unwrap();
        let e1 = source.next_event().await.unwrap().unwrap();
        assert_eq!(e1.text, "ok");
        let e2 = source.next_event().await.unwrap().unwrap();
        assert_eq!(e2.text, "also ok");

        std::fs::remove_file(&path).ok();
    }
}
