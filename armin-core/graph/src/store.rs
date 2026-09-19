use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Result};
use petgraph::Direction;
use petgraph::stable_graph::{EdgeIndex, NodeIndex, StableDiGraph};
use tokio::sync::RwLock;

use crate::communities;
use crate::debt::compute_debt;
use crate::decisions;
use crate::diff;
use crate::persistence::DbBackend;
use crate::risks;
use crate::summary;
use crate::types::{
    ArgumentEdge, ArgumentNode, CommunityReport, DebtReport, Decision, ExecutiveSummary, GraphDiff,
    GraphSnapshot, QueryResult, Risk,
};

pub type GraphIx = u32;

pub struct GraphStoreInner {
    pub graph: StableDiGraph<ArgumentNode, ArgumentEdge, GraphIx>,
    pub id_to_node: HashMap<String, NodeIndex<GraphIx>>,
    pub id_to_edge: HashMap<String, EdgeIndex<GraphIx>>,
    /// Insertion-ordered node IDs for "last N" queries
    pub node_order: VecDeque<String>,
    /// Insertion-ordered unique session IDs (drives recency/severity scoring)
    pub session_order: Vec<String>,
    /// Incremental BM25 index: node_id → term → frequency.
    /// Updated on insert; queries never rescan the whole graph.
    pub bm25_tf: HashMap<String, HashMap<String, usize>>,
    /// term → number of documents containing it
    pub bm25_df: HashMap<String, usize>,
    /// Sum of document lengths (token counts) for BM25's avgdl.
    pub bm25_total_len: usize,
}

impl GraphStoreInner {
    fn new() -> Self {
        Self {
            graph: StableDiGraph::default(),
            id_to_node: HashMap::new(),
            id_to_edge: HashMap::new(),
            node_order: VecDeque::new(),
            session_order: Vec::new(),
            bm25_tf: HashMap::new(),
            bm25_df: HashMap::new(),
            bm25_total_len: 0,
        }
    }

    /// Index a node's terms into the incremental BM25 structures.
    fn index_node(&mut self, node_id: &str, text: &str) {
        let mut tf: HashMap<String, usize> = HashMap::new();
        for t in tokenize(text) {
            *tf.entry(t).or_insert(0) += 1;
        }
        let doc_len = tf.values().sum::<usize>();
        self.bm25_total_len += doc_len;
        for term in tf.keys() {
            *self.bm25_df.entry(term.clone()).or_insert(0) += 1;
        }
        self.bm25_tf.insert(node_id.to_string(), tf);
    }
}

#[derive(Clone)]
pub struct GraphStore {
    inner: Arc<RwLock<GraphStoreInner>>,
    backend: Option<DbBackend>,
}

impl GraphStore {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(GraphStoreInner::new())),
            backend: None,
        }
    }

    /// Open a persistent graph store backed by sled at the given path.
    ///
    /// If the database already exists, all previously stored nodes and edges
    /// are loaded into memory and the petgraph is reconstructed from them.
    /// If it does not exist, an empty store is created and the directory is
    /// initialized on first write.
    pub fn new_persistent(path: impl AsRef<std::path::Path>) -> Result<Self> {
        let backend = DbBackend::open(path.as_ref())?;

        let nodes = backend.load_all_nodes()?;
        let edges = backend.load_all_edges()?;
        let loaded_order = backend.load_node_order()?;

        let mut graph = petgraph::stable_graph::StableDiGraph::default();
        let mut id_to_node: HashMap<String, petgraph::stable_graph::NodeIndex<GraphIx>> =
            HashMap::with_capacity(nodes.len());
        let mut id_to_edge: HashMap<String, petgraph::stable_graph::EdgeIndex<GraphIx>> =
            HashMap::with_capacity(edges.len());

        for node in &nodes {
            let idx = graph.add_node(node.clone());
            id_to_node.insert(node.id.clone(), idx);
        }

        for edge in &edges {
            let src = id_to_node
                .get(&edge.source_node_id)
                .ok_or_else(|| anyhow!("Persisted edge {} references missing source node {}", edge.id, edge.source_node_id))?;
            let tgt = id_to_node
                .get(&edge.target_node_id)
                .ok_or_else(|| anyhow!("Persisted edge {} references missing target node {}", edge.id, edge.target_node_id))?;
            let idx = graph.add_edge(*src, *tgt, edge.clone());
            id_to_edge.insert(edge.id.clone(), idx);
        }

        let node_order: VecDeque<String> = loaded_order
            .into_iter()
            .filter(|id| id_to_node.contains_key(id))
            .collect();

        // Reconstruct session_order from nodes in insertion order, and seed
        // the incremental BM25 index from the persisted nodes.
        let mut session_order: Vec<String> = Vec::new();
        for id in &node_order {
            if let Some(&idx) = id_to_node.get(id) {
                let sid = graph[idx].session_id.clone();
                if !session_order.contains(&sid) {
                    session_order.push(sid);
                }
            }
        }

        let mut inner = GraphStoreInner {
            graph,
            id_to_node,
            id_to_edge,
            node_order,
            session_order,
            bm25_tf: HashMap::with_capacity(nodes.len()),
            bm25_df: HashMap::new(),
            bm25_total_len: 0,
        };
        for node in &nodes {
            let text = format!("{} {}", node.label, node.description).to_lowercase();
            inner.index_node(&node.id, &text);
        }

        Ok(Self {
            inner: Arc::new(RwLock::new(inner)),
            backend: Some(backend),
        })
    }

    pub async fn add_node(&self, node: ArgumentNode) -> NodeIndex<GraphIx> {
        let mut inner = self.inner.write().await;
        if let Some(&existing) = inner.id_to_node.get(&node.id) {
            return existing;
        }
        let id = node.id.clone();
        let sid = node.session_id.clone();
        let node_for_backend = node.clone();
        // Seed the incremental BM25 index before moving the node into the graph.
        {
            let text = format!("{} {}", node.label, node.description).to_lowercase();
            inner.index_node(&id, &text);
        }
        let idx = inner.graph.add_node(node);
        inner.id_to_node.insert(id.clone(), idx);
        inner.node_order.push_back(id.clone());
        if !inner.session_order.contains(&sid) {
            inner.session_order.push(sid);
        }

        if let Some(ref backend) = self.backend {
            if let Err(e) = backend.store_node(&node_for_backend) {
                tracing::warn!("Failed to persist node {}: {e}", id);
            }
            // O(1): append a single index entry instead of re-serializing
            // the whole insertion order on every insert.
            if let Err(e) = backend.append_node_order(&id) {
                tracing::warn!("Failed to persist node order entry: {e}");
            }
        }

        idx
    }

    pub async fn add_edge(&self, edge: ArgumentEdge) -> Result<EdgeIndex<GraphIx>> {
        let mut inner = self.inner.write().await;
        let src_idx = *inner
            .id_to_node
            .get(&edge.source_node_id)
            .ok_or_else(|| anyhow!("source node {} not found", edge.source_node_id))?;
        let tgt_idx = *inner
            .id_to_node
            .get(&edge.target_node_id)
            .ok_or_else(|| anyhow!("target node {} not found", edge.target_node_id))?;

        if inner.id_to_edge.contains_key(&edge.id) {
            return Ok(*inner.id_to_edge.get(&edge.id).unwrap());
        }

        let id = edge.id.clone();
        let edge_for_backend = edge.clone();
        let idx = inner.graph.add_edge(src_idx, tgt_idx, edge);
        inner.id_to_edge.insert(id.clone(), idx);

        if let Some(ref backend) = self.backend {
            if let Err(e) = backend.store_edge(&edge_for_backend) {
                tracing::warn!("Failed to persist edge {}: {e}", id);
            }
        }

        Ok(idx)
    }

    /// Mark a node as Invalidated. Returns an error if the node doesn't exist.
    pub async fn invalidate_node(&self, node_id: &str) -> Result<()> {
        let mut inner = self.inner.write().await;
        let idx = inner
            .id_to_node
            .get(node_id)
            .copied()
            .ok_or_else(|| anyhow!("Node {node_id} not found"))?;
        let node = &mut inner.graph[idx];
        node.status = crate::types::NodeStatus::Invalidated;

        if let Some(ref backend) = self.backend {
            if let Err(e) = backend.store_node(&inner.graph[idx]) {
                tracing::warn!("Failed to persist invalidated node {}: {e}", node_id);
            }
        }
        Ok(())
    }

    /// Create a Resolves edge from `resolver_node_id` to `question_id`.
    /// Both nodes must exist. Returns the created edge.
    pub async fn resolve_question(
        &self,
        question_id: &str,
        resolver_node_id: &str,
        reasoning: &str,
    ) -> Result<ArgumentEdge> {
        let edge = {
            let inner = self.inner.read().await;
            if !inner.id_to_node.contains_key(question_id) {
                return Err(anyhow!("Question node {question_id} not found"));
            }
            if !inner.id_to_node.contains_key(resolver_node_id) {
                return Err(anyhow!("Resolver node {resolver_node_id} not found"));
            }

            let ts = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs_f64();
            ArgumentEdge {
                id: uuid::Uuid::new_v4().to_string(),
                edge_type: crate::types::EdgeType::Resolves,
                source_node_id: resolver_node_id.to_string(),
                target_node_id: question_id.to_string(),
                reasoning: reasoning.to_string(),
                timestamp: ts,
                evidence_score: None,
                provenance: crate::types::EdgeProvenance::Extracted,
            }
        };
        self.add_edge(edge.clone()).await?;
        Ok(edge)
    }

    pub async fn snapshot(&self) -> GraphSnapshot {
        let inner = self.inner.read().await;
        let nodes: Vec<ArgumentNode> = inner
            .graph
            .node_indices()
            .map(|i| inner.graph[i].clone())
            .collect();
        let edges: Vec<ArgumentEdge> = inner
            .graph
            .edge_indices()
            .map(|i| inner.graph[i].clone())
            .collect();
        GraphSnapshot { nodes, edges }
    }

    pub async fn snapshot_at_time(&self, max_time: f64) -> GraphSnapshot {
        let inner = self.inner.read().await;
        let nodes: Vec<ArgumentNode> = inner
            .graph
            .node_indices()
            .map(|i| inner.graph[i].clone())
            .filter(|n| n.timestamp <= max_time)
            .collect();
        let node_ids: std::collections::HashSet<&str> = nodes.iter().map(|n| n.id.as_str()).collect();
        let edges: Vec<ArgumentEdge> = inner
            .graph
            .edge_indices()
            .map(|i| inner.graph[i].clone())
            .filter(|e| {
                e.timestamp <= max_time
                    && node_ids.contains(e.source_node_id.as_str())
                    && node_ids.contains(e.target_node_id.as_str())
            })
            .collect();
        GraphSnapshot { nodes, edges }
    }

    pub async fn last_n_nodes(&self, n: usize) -> Vec<ArgumentNode> {
        let inner = self.inner.read().await;
        inner
            .node_order
            .iter()
            .rev()
            .take(n)
            .rev()
            .filter_map(|id| {
                inner
                    .id_to_node
                    .get(id)
                    .map(|&idx| inner.graph[idx].clone())
            })
            .collect()
    }

    /// Snapshot of the most recent `node_count` nodes plus up to
    /// `edge_count` edges touching them — without cloning the whole graph.
    /// Used to build extraction prompt context in O(k) instead of O(N).
    pub async fn recent_snapshot(&self, node_count: usize, edge_count: usize) -> GraphSnapshot {
        let inner = self.inner.read().await;
        let nodes: Vec<ArgumentNode> = inner
            .node_order
            .iter()
            .rev()
            .take(node_count)
            .filter_map(|id| inner.id_to_node.get(id).map(|&idx| inner.graph[idx].clone()))
            .collect();
        let node_ids: HashSet<&str> = nodes.iter().map(|n| n.id.as_str()).collect();
        let mut edges: Vec<ArgumentEdge> = Vec::new();
        // Walk edges touching the selected nodes, newest first.
        for id in inner.node_order.iter().rev().take(node_count * 2) {
            let Some(&idx) = inner.id_to_node.get(id) else { continue };
            for e in inner.graph.edges_directed(idx, Direction::Incoming) {
                let edge = e.weight();
                if node_ids.contains(edge.source_node_id.as_str())
                    && node_ids.contains(edge.target_node_id.as_str())
                    && !edges.iter().any(|x| x.id == edge.id)
                {
                    edges.push(edge.clone());
                    if edges.len() >= edge_count {
                        return GraphSnapshot { nodes, edges };
                    }
                }
            }
        }
        GraphSnapshot { nodes, edges }
    }

    /// Active OpenItem nodes that have no incoming Resolves edge, newest
    /// first, up to `limit`. These are the open items worth linking by the
    /// resolution-linking pass.
    pub async fn open_questions(&self, limit: usize) -> Vec<ArgumentNode> {
        let inner = self.inner.read().await;
        let mut out = Vec::new();
        for id in inner.node_order.iter().rev() {
            if out.len() >= limit {
                break;
            }
            let Some(&idx) = inner.id_to_node.get(id) else {
                continue;
            };
            let node = &inner.graph[idx];
            if node.node_type != crate::types::NodeType::OpenItem
                || node.status != crate::types::NodeStatus::Active
            {
                continue;
            }
            let already_resolved = inner
                .graph
                .edges_directed(idx, Direction::Incoming)
                .any(|e| e.weight().edge_type == crate::types::EdgeType::Resolves);
            if !already_resolved {
                out.push(node.clone());
            }
        }
        out
    }

    /// BFS from seed node IDs, up to `depth` hops in both directions.
    pub async fn bfs_subgraph(&self, seed_ids: &[String], depth: usize) -> GraphSnapshot {
        let inner = self.inner.read().await;
        let mut visited_nodes: std::collections::HashSet<NodeIndex<GraphIx>> =
            std::collections::HashSet::new();

        let seeds: Vec<NodeIndex<GraphIx>> = seed_ids
            .iter()
            .filter_map(|id| inner.id_to_node.get(id).copied())
            .collect();

        let mut frontier = seeds;
        visited_nodes.extend(frontier.iter().copied());

        for _ in 0..depth {
            let mut next_frontier = Vec::new();
            for &node_idx in &frontier {
                for neighbor in inner
                    .graph
                    .neighbors_directed(node_idx, Direction::Incoming)
                    .chain(inner.graph.neighbors_directed(node_idx, Direction::Outgoing))
                {
                    if visited_nodes.insert(neighbor) {
                        next_frontier.push(neighbor);
                    }
                }
            }
            frontier = next_frontier;
        }

        let nodes: Vec<ArgumentNode> = visited_nodes
            .iter()
            .map(|&i| inner.graph[i].clone())
            .collect();
        let node_ids: std::collections::HashSet<&str> =
            nodes.iter().map(|n| n.id.as_str()).collect();
        let edges: Vec<ArgumentEdge> = inner
            .graph
            .edge_indices()
            .map(|i| inner.graph[i].clone())
            .filter(|e| {
                node_ids.contains(e.source_node_id.as_str())
                    && node_ids.contains(e.target_node_id.as_str())
            })
            .collect();

        GraphSnapshot { nodes, edges }
    }

    pub async fn compute_debt_report(&self, current_session_idx: usize) -> DebtReport {
        let inner = self.inner.read().await;
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();
        compute_debt(&inner, current_session_idx, ts)
    }

    pub async fn compute_community_report(&self) -> CommunityReport {
        let inner = self.inner.read().await;
        communities::detect_communities(&inner)
    }

    pub async fn extract_decisions(&self, current_session_idx: usize) -> Vec<Decision> {
        let inner = self.inner.read().await;
        decisions::extract_decisions(&inner, current_session_idx)
    }

    pub async fn compute_risks(&self, current_session_idx: usize) -> Vec<Risk> {
        let inner = self.inner.read().await;
        risks::compute_risks(&inner, current_session_idx)
    }

    pub async fn compute_summary(
        &self,
        current_session_idx: usize,
        prior_debt: Option<&DebtReport>,
    ) -> ExecutiveSummary {
        let inner = self.inner.read().await;
        summary::compute_summary(&inner, current_session_idx, prior_debt)
    }

    pub async fn compute_diff(&self, from_time: f64, to_time: f64) -> GraphDiff {
        let before = self.snapshot_at_time(from_time).await;
        let after = self.snapshot_at_time(to_time).await;
        diff::compute_graph_diff(&before, &after)
    }

    pub async fn node_count(&self) -> usize {
        self.inner.read().await.graph.node_count()
    }

    pub async fn edge_count(&self) -> usize {
        self.inner.read().await.graph.edge_count()
    }

    pub async fn get_node(&self, id: &str) -> Option<ArgumentNode> {
        let inner = self.inner.read().await;
        inner
            .id_to_node
            .get(id)
            .map(|&idx| inner.graph[idx].clone())
    }

    /// Flush the underlying database to disk.
    /// No-op for in-memory stores.
    pub async fn flush(&self) -> Result<()> {
        if let Some(ref backend) = self.backend {
            backend.flush()?;
        }
        Ok(())
    }

    pub async fn all_node_labels(&self) -> Vec<(String, String)> {
        let inner = self.inner.read().await;
        inner
            .graph
            .node_indices()
            .map(|i| {
                let n = &inner.graph[i];
                (n.id.clone(), n.label.clone())
            })
            .collect()
    }

    /// BM25-style keyword scoring + centrality boost to find relevant nodes.
    /// Uses the incremental index (updated on insert) — no full-graph rescan.
    pub async fn find_relevant_nodes_bm25(&self, question: &str, top_n: usize) -> Vec<String> {
        let inner = self.inner.read().await;
        let query_tokens = tokenize(question);
        if query_tokens.is_empty() {
            return vec![];
        }

        let n_docs = inner.bm25_tf.len() as f64;
        if n_docs == 0.0 {
            return vec![];
        }

        let k1 = 1.2_f64;
        let b = 0.75_f64;
        let avgdl = (inner.bm25_total_len as f64 / n_docs).max(1.0);

        let mut scores: Vec<(String, f64)> = Vec::with_capacity(inner.bm25_tf.len());
        for (id, tf) in &inner.bm25_tf {
            let doc_len = tf.values().sum::<usize>() as f64;
            let mut score = 0.0_f64;
            for term in &query_tokens {
                let n_q = *inner.bm25_df.get(term).unwrap_or(&0) as f64;
                if n_q == 0.0 {
                    continue;
                }
                let idf = ((n_docs - n_q + 0.5) / (n_q + 0.5) + 1.0).ln();
                let freq = *tf.get(term).unwrap_or(&0) as f64;
                let numerator = freq * (k1 + 1.0);
                let denominator = freq + k1 * (1.0 - b + b * doc_len / avgdl);
                score += idf * (numerator / denominator);
            }

            let node_idx = inner.id_to_node.get(id).copied();
            if let Some(idx) = node_idx {
                let in_degree = inner.graph.neighbors_directed(idx, Direction::Incoming).count();
                let out_degree = inner.graph.neighbors_directed(idx, Direction::Outgoing).count();
                let degree = (in_degree + out_degree) as f64;
                score += degree * 0.1;
            }

            scores.push((id.clone(), score));
        }

        scores.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        scores.into_iter().take(top_n).map(|(id, _)| id).collect()
    }

    /// Deterministic query: finds the answer node via BM25, then traces backwards
    /// through incoming edges to build a reasoning chain (evidence → claim → decision).
    pub async fn query_subgraph(&self, question: &str, subgraph: &GraphSnapshot) -> QueryResult {
        if subgraph.nodes.is_empty() {
            return QueryResult {
                answer: "The graph does not yet contain enough information to answer this question.".to_string(),
                trace: vec![],
                cited_events: vec![],
                mode_used: "deterministic".to_string(),
            };
        }

        let query_tokens = tokenize(question);

        let mut node_scores: Vec<(&ArgumentNode, f64)> = subgraph
            .nodes
            .iter()
            .map(|node| {
                let text = format!("{} {}", node.label, node.description).to_lowercase();
                let tokens = tokenize(&text);
                let mut score = 0.0_f64;
                for term in &query_tokens {
                    let count = tokens.iter().filter(|t| t == &term).count() as f64;
                    score += count;
                }
                (node, score)
            })
            .collect();

        node_scores.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        // Build reverse adjacency: target -> [(source, edge)] for tracing backwards
        let node_ids: HashSet<&str> = subgraph.nodes.iter().map(|n| n.id.as_str()).collect();
        let mut reverse_adj: HashMap<&str, Vec<(&str, &ArgumentEdge)>> = HashMap::new();
        for edge in &subgraph.edges {
            if node_ids.contains(edge.source_node_id.as_str())
                && node_ids.contains(edge.target_node_id.as_str())
            {
                reverse_adj
                    .entry(edge.target_node_id.as_str())
                    .or_default()
                    .push((edge.source_node_id.as_str(), edge));
            }
        }

        // Find the best answer node (highest keyword match)
        let seed_nodes: Vec<&ArgumentNode> = node_scores
            .iter()
            .filter(|(_, score)| *score > 0.0)
            .map(|(node, _)| *node)
            .take(3)
            .collect();

        let mut trace: Vec<String> = Vec::new();
        let mut visited: HashSet<String> = HashSet::new();
        let mut cited_events: Vec<String> = Vec::new();

        if seed_nodes.is_empty() {
            let fallback: Vec<&ArgumentNode> = node_scores.iter().take(3).map(|(n, _)| *n).collect();
            for node in &fallback {
                if visited.insert(node.id.clone()) {
                    trace.push(node.id.clone());
                    cited_events.push(node.event_id.clone());
                }
            }
        } else {
            for seed in seed_nodes {
                if visited.contains(&seed.id) {
                    continue;
                }
                // Trace backwards from the answer node through incoming edges
                // This follows: "What evidence/claims support this conclusion?"
                let mut stack: Vec<(&ArgumentNode, Option<&ArgumentEdge>)> = vec![(seed, None)];
                while let Some((current, _edge_in)) = stack.pop() {
                    if visited.contains(&current.id) {
                        continue;
                    }
                    visited.insert(current.id.clone());
                    trace.push(current.id.clone());
                    cited_events.push(current.event_id.clone());

                    if let Some(neighbors) = reverse_adj.get(current.id.as_str()) {
                        let mut sorted: Vec<_> = neighbors.to_vec();
                        sorted.sort_by_key(|(_, edge)| edge.edge_type.preference_order());
                        for (neighbor_id, edge) in sorted {
                            if !visited.contains(neighbor_id) {
                                if let Some(node) = subgraph.nodes.iter().find(|n| n.id == neighbor_id) {
                                    stack.push((node, Some(edge)));
                                }
                            }
                        }
                    }
                }
            }
        }

        cited_events = cited_events.into_iter().collect::<HashSet<_>>().into_iter().collect();
        cited_events.sort();

        QueryResult {
            answer: String::new(),
            trace,
            cited_events,
            mode_used: "deterministic".to_string(),
        }
    }
}

impl Default for GraphStore {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for GraphStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GraphStore")
            .field("has_backend", &self.backend.is_some())
            .finish()
    }
}

pub use crate::utils::tokenize;
