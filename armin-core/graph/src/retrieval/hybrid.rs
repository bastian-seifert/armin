use std::collections::HashSet;

use async_trait::async_trait;

use crate::{GraphStore, NodeRetriever};

use super::{Bm25Retriever, EmbeddingRetriever};

pub struct HybridRetriever {
    _store: GraphStore,
    bm25: Bm25Retriever,
    embedding: EmbeddingRetriever,
}

impl HybridRetriever {
    pub fn new(store: GraphStore, api_key: String) -> Self {
        Self {
            _store: store.clone(),
            bm25: Bm25Retriever::new(store.clone()),
            embedding: EmbeddingRetriever::new(store, api_key),
        }
    }
}

#[async_trait]
impl NodeRetriever for HybridRetriever {
    async fn find_relevant_nodes(&self, question: &str, top_n: usize) -> Vec<String> {
        let double_n = top_n * 2;
        let (bm25_ids, emb_ids) = tokio::join!(
            self.bm25.find_relevant_nodes(question, double_n),
            self.embedding.find_relevant_nodes(question, double_n),
        );

        let mut seen = HashSet::new();
        let mut merged = Vec::new();
        for id in bm25_ids.into_iter().chain(emb_ids) {
            if seen.insert(id.clone()) {
                merged.push(id);
            }
        }
        merged.truncate(top_n);
        merged
    }
}
