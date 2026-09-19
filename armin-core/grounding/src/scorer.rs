use crate::EvidenceSource;

/// Computes an aggregate evidence score (0 – 1) for an edge's reasoning based on
/// search results. Combines relevance, credibility, and freshness.
pub struct EvidenceScorer;

impl EvidenceScorer {
    /// Score a single evidence source against the original query.
    /// Returns a value in [0, 1] where higher is more trustworthy.
    pub fn score(query: &str, source: &EvidenceSource) -> f64 {
        let relevance = Self::relevance_score(query, source);
        let credibility = source.credibility_score;
        // Equal weights for relevance and credibility.
        0.5 * relevance + 0.5 * credibility
    }

    /// Aggregate the top-N sources into a single score.
    pub fn aggregate(query: &str, sources: &[EvidenceSource]) -> f64 {
        if sources.is_empty() {
            return 0.0;
        }
        let mut scored: Vec<f64> = sources
            .iter()
            .map(|s| Self::score(query, s))
            .collect();
        scored.sort_by(|a, b| b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal));
        // Weight: top result 50%, second 30%, third 20%.
        let top3: &[f64] = if scored.len() >= 3 {
            &scored[..3]
        } else {
            &scored
        };
        let weights: Vec<f64> = match top3.len() {
            1 => vec![1.0],
            2 => vec![0.6, 0.4],
            _ => vec![0.5, 0.3, 0.2],
        };
        top3
            .iter()
            .zip(weights.iter())
            .map(|(s, w)| s * w)
            .sum()
    }

    /// Measure keyword overlap between the query and the source's title + snippet.
    fn relevance_score(query: &str, source: &EvidenceSource) -> f64 {
        let query_lower = query.to_lowercase();
        let query_words: Vec<&str> = query_lower
            .split_whitespace()
            .filter(|w| w.len() > 3)
            .collect();
        if query_words.is_empty() {
            return 0.5;
        }
        let text = format!("{} {}", source.title, source.snippet);
        let text_lower = text.to_lowercase();
        let matches = query_words
            .iter()
            .filter(|w| text_lower.contains(*w))
            .count();
        matches as f64 / query_words.len() as f64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(title: &str, snippet: &str, credibility: f64) -> EvidenceSource {
        EvidenceSource {
            url: "https://example.com".into(),
            title: title.into(),
            snippet: snippet.into(),
            credibility_score: credibility,
        }
    }

    #[test]
    fn test_empty_sources_returns_zero() {
        let score = EvidenceScorer::aggregate("test query", &[]);
        assert_eq!(score, 0.0);
    }

    #[test]
    fn test_single_source_uses_full_score() {
        let src = source("Climate Change Report", "Global warming data", 0.8);
        let score = EvidenceScorer::aggregate("climate change", &[src]);
        assert!(score > 0.0);
        assert!(score <= 1.0);
    }

    #[test]
    fn test_relevance_short_query_returns_half() {
        let src = source("A", "B", 1.0);
        let score = EvidenceScorer::relevance_score("a b", &src);
        assert!((score - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn test_relevance_keyword_match() {
        let src = source("Climate Report", "Global climate data", 0.5);
        let score = EvidenceScorer::relevance_score("climate global", &src);
        assert!(score > 0.0);
    }

    #[test]
    fn test_aggregate_top3_weighting() {
        let sources = vec![
            source("Title A", "Snippet about climate", 0.9),
            source("Title B", "More climate data here", 0.8),
            source("Title C", "Climate research paper", 0.7),
        ];
        let score = EvidenceScorer::aggregate("climate", &sources);
        assert!(score > 0.0);
        assert!(score <= 1.0);
    }

    #[test]
    fn test_creditibility_high_domain() {
        let src = source("Article", "Content", 0.95);
        let score = EvidenceScorer::score("test", &src);
        assert!(score > 0.4);
    }
}
