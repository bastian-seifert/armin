use async_trait::async_trait;
use serde::Deserialize;

/// A piece of external evidence retrieved from a search.
#[derive(Clone, Debug)]
pub struct EvidenceSource {
    pub url: String,
    pub title: String,
    pub snippet: String,
    /// Pre-computed credibility score in [0, 1] based on domain authority.
    pub credibility_score: f64,
}

#[async_trait]
pub trait EvidenceSearcher: Send + Sync {
    async fn search(&self, query: &str) -> anyhow::Result<Vec<EvidenceSource>>;
}

/// Searches the web via the Brave Search API.
pub struct BraveSearcher {
    client: reqwest::Client,
    api_key: String,
}

impl BraveSearcher {
    pub fn new(api_key: String) -> Self {
        Self {
            client: reqwest::Client::new(),
            api_key,
        }
    }
}

#[async_trait]
impl EvidenceSearcher for BraveSearcher {
    async fn search(&self, query: &str) -> anyhow::Result<Vec<EvidenceSource>> {
        let resp = self
            .client
            .get("https://api.search.brave.com/res/v1/web/search")
            .header("X-Subscription-Token", &self.api_key)
            .header("Accept", "application/json")
            .query(&[("q", query), ("count", "5")])
            .send()
            .await?;

        let body: BraveResponse = resp.json().await?;

        let sources: Vec<EvidenceSource> = body
            .web
            .map(|web| {
                web.results
                    .into_iter()
                    .map(|r| {
                        let cred = domain_credibility(&r.url);
                        EvidenceSource {
                            url: r.url,
                            title: r.title,
                            snippet: r.description,
                            credibility_score: cred,
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();

        Ok(sources)
    }
}

/// Rough domain-credibility heuristic based on known high-quality sources.
fn domain_credibility(url: &str) -> f64 {
    let url_lower = url.to_lowercase();
    if url_lower.contains("wikipedia.org") || url_lower.contains("nature.com") {
        0.95
    } else if url_lower.contains(".gov") || url_lower.contains(".edu") {
        0.85
    } else if url_lower.contains("reuters.com")
        || url_lower.contains("apnews.com")
        || url_lower.contains("bbc.com")
    {
        0.80
    } else if url_lower.contains("nytimes.com")
        || url_lower.contains("wsj.com")
        || url_lower.contains("economist.com")
    {
        0.75
    } else {
        0.50
    }
}

// ── Brave API response shapes ──────────────────────────────────────────────────

#[derive(Deserialize)]
struct BraveResponse {
    web: Option<WebResults>,
}

#[derive(Deserialize)]
struct WebResults {
    results: Vec<BraveResult>,
}

#[derive(Deserialize)]
struct BraveResult {
    url: String,
    title: String,
    description: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_domain_credibility_wikipedia() {
        let score = domain_credibility("https://en.wikipedia.org/wiki/Climate_change");
        assert!((score - 0.95).abs() < f64::EPSILON);
    }

    #[test]
    fn test_domain_credibility_gov() {
        let score = domain_credibility("https://www.nasa.gov/climate");
        assert!((score - 0.85).abs() < f64::EPSILON);
    }

    #[test]
    fn test_domain_credibility_edu() {
        let score = domain_credibility("https://climate.mit.edu/");
        assert!((score - 0.85).abs() < f64::EPSILON);
    }

    #[test]
    fn test_domain_credibility_reuters() {
        let score = domain_credibility("https://www.reuters.com/world/climate/");
        assert!((score - 0.80).abs() < f64::EPSILON);
    }

    #[test]
    fn test_domain_credibility_default() {
        let score = domain_credibility("https://blog.example.com/post");
        assert!((score - 0.50).abs() < f64::EPSILON);
    }
}
