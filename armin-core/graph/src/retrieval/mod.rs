pub mod embedding;
pub mod hybrid;

pub use embedding::EmbeddingRetriever;
pub use hybrid::HybridRetriever;

use async_trait::async_trait;

use crate::GraphStore;

#[async_trait]
pub trait NodeRetriever: Send + Sync {
    async fn find_relevant_nodes(&self, question: &str, top_n: usize) -> Vec<String>;
}

pub struct Bm25Retriever {
    store: GraphStore,
}

impl Bm25Retriever {
    pub fn new(store: GraphStore) -> Self {
        Self { store }
    }
}

#[async_trait]
impl NodeRetriever for Bm25Retriever {
    async fn find_relevant_nodes(&self, question: &str, top_n: usize) -> Vec<String> {
        self.store.find_relevant_nodes_bm25(question, top_n).await
    }
}
