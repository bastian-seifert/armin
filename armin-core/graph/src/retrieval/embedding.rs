use std::sync::Arc;

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use tokio::sync::RwLock;

use crate::{ArgumentNode, GraphStore, NodeRetriever};

pub struct EmbeddingRetriever {
    store: GraphStore,
    index: Arc<RwLock<Vec<(String, Vec<f32>)>>>,
    api_key: String,
    model: String,
    rebuild_on_query: Arc<std::sync::atomic::AtomicBool>,
}

impl EmbeddingRetriever {
    pub fn new(store: GraphStore, api_key: String) -> Self {
        let model = std::env::var("EMBEDDING_MODEL")
            .unwrap_or_else(|_| "text-embedding-3-small".to_string());
        Self {
            store,
            index: Arc::new(RwLock::new(Vec::new())),
            api_key,
            model,
            rebuild_on_query: Arc::new(std::sync::atomic::AtomicBool::new(true)),
        }
    }

    fn text_for_node(node: &ArgumentNode) -> String {
        format!("{} {} {:?} {}", node.label, node.description, node.node_type, node.agent_id)
    }

    pub async fn rebuild_index(&self) -> Result<()> {
        let snapshot = self.store.snapshot().await;
        if snapshot.nodes.is_empty() {
            *self.index.write().await = Vec::new();
            return Ok(());
        }

        let texts: Vec<(String, String)> = snapshot.nodes.iter()
            .map(|n| (n.id.clone(), Self::text_for_node(n)))
            .collect();

        let embeddings = self.embed_batch(&texts).await?;
        *self.index.write().await = embeddings;
        self.rebuild_on_query.store(false, std::sync::atomic::Ordering::Relaxed);
        tracing::info!("Rebuilt embedding index with {} nodes", snapshot.nodes.len());
        Ok(())
    }

    pub async fn upsert_node(&self, node: &ArgumentNode) -> Result<()> {
        let text = Self::text_for_node(node);
        let embeddings = self.embed_batch(&[(node.id.clone(), text)]).await?;
        if let Some((id, emb)) = embeddings.into_iter().next() {
            let mut index = self.index.write().await;
            if let Some(pos) = index.iter().position(|(nid, _)| nid == &id) {
                index[pos] = (id, emb);
            } else {
                index.push((id, emb));
            }
        }
        Ok(())
    }

    pub async fn remove_node(&self, node_id: &str) {
        let mut index = self.index.write().await;
        index.retain(|(id, _)| id != node_id);
    }

    fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
        let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
        let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
        let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm_a == 0.0 || norm_b == 0.0 { 0.0 } else { dot / (norm_a * norm_b) }
    }

    async fn embed_text(&self, text: &str) -> Result<Vec<f32>> {
        let url = "https://api.openai.com/v1/embeddings";
        let body = serde_json::json!({
            "model": self.model,
            "input": text,
        });

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()?;

        let resp = client.post(url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .json(&body)
            .send()
            .await?;

        let status = resp.status();
        let data: serde_json::Value = resp.json().await?;
        if !status.is_success() {
            return Err(anyhow!("OpenAI embedding API error {}: {}", status, data));
        }

        let embedding = data["data"][0]["embedding"]
            .as_array()
            .ok_or_else(|| anyhow!("Missing embedding in response"))?
            .iter()
            .map(|v| v.as_f64().unwrap_or(0.0) as f32)
            .collect();

        Ok(embedding)
    }

    async fn embed_batch(&self, texts: &[(String, String)]) -> Result<Vec<(String, Vec<f32>)>> {
        let url = "https://api.openai.com/v1/embeddings";
        let input: Vec<&str> = texts.iter().map(|(_, t)| t.as_str()).collect();
        let body = serde_json::json!({
            "model": self.model,
            "input": input,
        });

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(60))
            .build()?;

        let resp = client.post(url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .json(&body)
            .send()
            .await?;

        let status = resp.status();
        let data: serde_json::Value = resp.json().await?;
        if !status.is_success() {
            return Err(anyhow!("OpenAI embedding API error {}: {}", status, data));
        }

        let mut results = Vec::with_capacity(texts.len());
        if let Some(arr) = data["data"].as_array() {
            for item in arr {
                let idx = item["index"].as_u64().unwrap_or(0) as usize;
                if idx < texts.len() {
                    let embedding: Vec<f32> = item["embedding"]
                        .as_array()
                        .map(|a| a.iter().map(|v| v.as_f64().unwrap_or(0.0) as f32).collect())
                        .unwrap_or_default();
                    results.push((texts[idx].0.clone(), embedding));
                }
            }
        }

        Ok(results)
    }
}

#[async_trait]
impl NodeRetriever for EmbeddingRetriever {
    async fn find_relevant_nodes(&self, question: &str, top_n: usize) -> Vec<String> {
        if self.rebuild_on_query.load(std::sync::atomic::Ordering::Relaxed) {
            if let Err(e) = self.rebuild_index().await {
                tracing::warn!("Embedding index rebuild failed on query: {e}, falling back to BM25");
                return self.store.find_relevant_nodes_bm25(question, top_n).await;
            }
        }

        let query_embedding = match self.embed_text(question).await {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!("Embedding query failed: {e}, falling back to BM25");
                return self.store.find_relevant_nodes_bm25(question, top_n).await;
            }
        };

        let index = self.index.read().await;
        if index.is_empty() {
            return self.store.find_relevant_nodes_bm25(question, top_n).await;
        }

        let mut scored: Vec<(String, f32)> = index.iter()
            .map(|(id, emb)| {
                let sim = Self::cosine_similarity(&query_embedding, emb);
                (id.clone(), sim)
            })
            .collect();

        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        scored.into_iter().take(top_n).map(|(id, _)| id).collect()
    }
}
