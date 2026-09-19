const STOPWORDS: &[&str] = &[
    "the", "a", "an", "is", "are", "was", "were", "be", "been", "being",
    "have", "has", "had", "do", "does", "did", "will", "would", "could",
    "should", "may", "might", "shall", "can", "to", "of", "in", "for",
    "on", "with", "at", "by", "from", "as", "into", "through", "during",
    "before", "after", "above", "below", "between", "under", "again",
    "further", "then", "once", "here", "there", "when", "where", "why",
    "how", "all", "each", "few", "more", "most", "other", "some", "such",
    "no", "nor", "not", "only", "own", "same", "so", "than", "too",
    "very", "just", "because", "but", "and", "or", "if", "while", "about",
    "what", "which", "who", "whom", "this", "that", "these", "those",
    "it", "its", "they", "them", "their", "we", "our", "you", "your",
    "he", "she", "his", "her", "i", "me", "my",
];

pub fn tokenize(text: &str) -> Vec<String> {
    text.split_whitespace()
        .map(|w| {
            w.chars()
                .filter(|c| c.is_alphanumeric())
                .collect::<String>()
                .to_lowercase()
        })
        .filter(|w| !w.is_empty() && !STOPWORDS.contains(&w.as_str()))
        .collect()
}

/// Position of a session_id within the insertion-ordered session list.
pub fn session_index(sessions: &[String], session_id: &str) -> Option<usize> {
    sessions.iter().position(|s| s == session_id)
}

/// Human-readable short label for a session: `S{n}` (1-indexed) or the raw id if unknown.
pub fn session_short_name(sessions: &[String], session_id: &str) -> String {
    match session_index(sessions, session_id) {
        Some(i) => format!("S{}", i + 1),
        None => session_id.to_string(),
    }
}

/// Session id at a given index, if present.
pub fn session_id_at_index(sessions: &[String], idx: usize) -> Option<String> {
    sessions.get(idx).cloned()
}
