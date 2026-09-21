//! Incremental reader for Claude Code session transcripts (`transcript_path`
//! from hook input): extracts the assistant's prose messages that appeared
//! after a byte-offset cursor, so extraction sees the same material the
//! OpenCode plugin captures from message events.

use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::Path;

/// Result of one incremental transcript read.
pub struct TranscriptTail {
    /// Assistant text blocks, in transcript order.
    pub texts: Vec<String>,
    /// Byte offset to resume from next time (after the last processed line).
    pub new_cursor: u64,
    /// True when the file shrank below the cursor (transcript replaced) and
    /// the read was restarted from zero.
    pub reset: bool,
}

/// One JSONL entry of a Claude Code transcript. Only the fields the reader
/// needs; everything else is ignored.
#[derive(serde::Deserialize)]
struct Entry {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    message: Option<Message>,
    #[serde(rename = "isSidechain", default)]
    is_sidechain: bool,
}

#[derive(serde::Deserialize)]
struct Message {
    #[serde(default)]
    content: serde_json::Value,
}

/// Extract text blocks from a message `content` value (array of blocks or a
/// bare string).
fn texts_from_content(content: &serde_json::Value, out: &mut Vec<String>) {
    match content {
        serde_json::Value::String(s) => {
            if !s.trim().is_empty() {
                out.push(s.clone());
            }
        }
        serde_json::Value::Array(blocks) => {
            for block in blocks {
                if block.get("type").and_then(|t| t.as_str()) == Some("text") {
                    if let Some(t) = block.get("text").and_then(|t| t.as_str()) {
                        if !t.trim().is_empty() {
                            out.push(t.to_string());
                        }
                    }
                }
            }
        }
        _ => {}
    }
}

/// Read assistant prose from `path` starting at byte offset `cursor`.
///
/// Returns up to roughly `max_texts` texts (the soft cap is applied per line,
/// so a line with several text blocks may push the result slightly over it —
/// the cursor always lands after the last processed line, never mid-line, and
/// unread overflow is picked up on the next call). A file shorter than
/// `cursor` (replaced transcript) restarts the read from zero and reports
/// `reset`.
pub fn read_new(path: &Path, cursor: u64, max_texts: usize) -> Option<TranscriptTail> {
    let mut file = std::fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    let mut offset = cursor;
    let mut reset = false;
    if len < cursor {
        offset = 0;
        reset = true;
    }
    file.seek(SeekFrom::Start(offset)).ok()?;

    let mut reader = BufReader::new(file);
    let mut texts: Vec<String> = Vec::new();
    let mut line = Vec::new();

    loop {
        line.clear();
        let read = reader.read_until(b'\n', &mut line).ok()?;
        if read == 0 {
            break;
        }
        if line.last() != Some(&b'\n') {
            // Partially written final line — stop before it; it completes by
            // the next call.
            break;
        }
        offset += line.len() as u64;

        if !line.starts_with(b"{") {
            continue;
        }
        let Ok(entry) = serde_json::from_slice::<Entry>(&line) else {
            continue;
        };
        if entry.is_sidechain || entry.kind != "assistant" {
            continue;
        }
        let mut block_texts = Vec::new();
        if let Some(message) = &entry.message {
            texts_from_content(&message.content, &mut block_texts);
        }
        texts.extend(block_texts);
        if texts.len() >= max_texts {
            return Some(TranscriptTail { texts, new_cursor: offset, reset });
        }
    }

    Some(TranscriptTail { texts, new_cursor: offset, reset })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_transcript(dir: &Path, contents: &str) -> std::path::PathBuf {
        let path = dir.join("session.jsonl");
        std::fs::write(&path, contents).unwrap();
        path
    }

    const LINE1: &str = r#"{"type":"user","message":{"role":"user","content":"hi"}}"#;
    const LINE2: &str = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"first answer"},{"type":"tool_use","name":"Read"}]}}"#;
    const LINE3: &str = r#"{"type":"assistant","isSidechain":true,"message":{"role":"assistant","content":[{"type":"text","text":"sidechain prose"}]}}"#;
    const LINE4: &str = r#"{"type":"summary","summary":"older summary"}"#;
    const LINE5: &str = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"second answer"}]}}"#;

    fn sample() -> String {
        format!("{LINE1}\n{LINE2}\n{LINE3}\n{LINE4}\n{LINE5}\n")
    }

    #[test]
    fn extracts_assistant_text_only() {
        let dir = tempfile::tempdir().unwrap();
        let contents = sample();
        let path = write_transcript(dir.path(), &contents);
        let tail = read_new(&path, 0, 100).unwrap();
        assert_eq!(tail.texts, vec!["first answer", "second answer"]);
        assert!(!tail.reset);
        assert_eq!(tail.new_cursor as usize, contents.len());
    }

    #[test]
    fn incremental_read_picks_up_new_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.jsonl");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(
            f,
            r#"{{"type":"assistant","message":{{"content":[{{"type":"text","text":"a"}}]}}}}"#
        )
        .unwrap();
        drop(f);
        let first = read_new(&path, 0, 100).unwrap();
        assert_eq!(first.texts, vec!["a"]);

        let mut f = std::fs::OpenOptions::new().append(true).open(&path).unwrap();
        writeln!(
            f,
            r#"{{"type":"assistant","message":{{"content":[{{"type":"text","text":"b"}}]}}}}"#
        )
        .unwrap();
        drop(f);

        let second = read_new(&path, first.new_cursor, 100).unwrap();
        assert_eq!(second.texts, vec!["b"]);
    }

    #[test]
    fn max_texts_defers_remainder() {
        let dir = tempfile::tempdir().unwrap();
        let contents = sample();
        let path = write_transcript(dir.path(), &contents);
        let first = read_new(&path, 0, 1).unwrap();
        assert_eq!(first.texts.len(), 1);
        assert_eq!(first.texts[0], "first answer");
        let second = read_new(&path, first.new_cursor, 100).unwrap();
        assert_eq!(second.texts, vec!["second answer"]);
    }

    #[test]
    fn partial_final_line_is_deferred() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.jsonl");
        let line1 = format!("{LINE2}\n");
        let partial = r#"{"type":"assi"#;
        std::fs::write(&path, format!("{line1}{partial}")).unwrap();
        let tail = read_new(&path, 0, 100).unwrap();
        assert_eq!(tail.texts, vec!["first answer"]);
        assert_eq!(tail.new_cursor as usize, line1.len());
    }

    #[test]
    fn shrunk_file_resets_cursor() {
        let dir = tempfile::tempdir().unwrap();
        let contents = format!(r#"{{"type":"assistant","message":{{"content":"fresh"}}}}"#);
        let contents = format!("{contents}\n");
        let path = write_transcript(dir.path(), &contents);
        let tail = read_new(&path, 10_000, 100).unwrap();
        assert!(tail.reset);
        assert_eq!(tail.texts, vec!["fresh"]);
    }
}
