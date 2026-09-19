pub mod communities;
pub mod debt;
pub mod decisions;
pub mod diff;
pub mod persistence;
pub mod retrieval;
pub mod risks;
pub mod store;
pub mod summary;
pub mod types;
pub mod utils;

pub use communities::detect_communities;
pub use persistence::DbBackend;
pub use retrieval::{Bm25Retriever, EmbeddingRetriever, HybridRetriever, NodeRetriever};
pub use store::GraphStore;
pub use types::{
    AgentDecisionRequest, AgentInvalidateRequest, AgentQuestionRequest, AgentResolveRequest,
    AgentWriteResponse, ArgumentEdge, ArgumentNode, ChangedNode, Community, CommunityReport,
    DebtDelta, DebtItem, DebtReport, Decision, EdgeProvenance, EdgeType, ExecutiveSummary,
    ExtractionResult, GraphDiff, GraphSnapshot, NodeStatus, NodeType, QueryResult, Risk,
    ScratchCheck, ScratchEdit, ScratchSnapshot, SurprisingConnection,
};
pub use utils::{session_id_at_index, session_index, session_short_name};

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn now_ts() -> f64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs_f64()
    }

    fn make_node(id: &str, node_type: NodeType, session_id: &str) -> ArgumentNode {
        ArgumentNode {
            id: id.to_string(),
            node_type,
            label: format!("Label for {id}"),
            description: format!("Description for {id}"),
            event_id: "evt-1".to_string(),
            agent_id: "PM".to_string(),
            session_id: session_id.to_string(),
            timestamp: now_ts(),
            confidence: 0.9,
            files: vec![],
            commit: None,
            mention_count: 1,
            status: NodeStatus::Active,
        }
    }

    fn make_edge(id: &str, edge_type: EdgeType, src: &str, tgt: &str) -> ArgumentEdge {
        ArgumentEdge {
            id: id.to_string(),
            edge_type,
            source_node_id: src.to_string(),
            target_node_id: tgt.to_string(),
            reasoning: "test".to_string(),
            timestamp: now_ts(),
            evidence_score: None,
            provenance: EdgeProvenance::default(),
        }
    }

    #[tokio::test]
    async fn test_add_and_retrieve_node() {
        let store = GraphStore::new();
        let node = make_node("n1", NodeType::Decision, "s1");
        store.add_node(node.clone()).await;
        let retrieved = store.get_node("n1").await.unwrap();
        assert_eq!(retrieved.id, "n1");
        assert_eq!(retrieved.node_type, NodeType::Decision);
    }

    #[tokio::test]
    async fn test_add_duplicate_node_is_idempotent() {
        let store = GraphStore::new();
        let node = make_node("n1", NodeType::Decision, "s1");
        store.add_node(node.clone()).await;
        store.add_node(node.clone()).await;
        assert_eq!(store.node_count().await, 1);
    }

    #[tokio::test]
    async fn test_add_edge_requires_existing_nodes() {
        let store = GraphStore::new();
        let edge = make_edge("e1", EdgeType::RelatesTo, "n1", "n2");
        assert!(store.add_edge(edge).await.is_err());
    }

    #[tokio::test]
    async fn test_snapshot_contains_all_nodes_and_edges() {
        let store = GraphStore::new();
        store.add_node(make_node("n1", NodeType::Decision, "s1")).await;
        store.add_node(make_node("n2", NodeType::Decision, "s1")).await;
        store.add_edge(make_edge("e1", EdgeType::RelatesTo, "n2", "n1")).await.unwrap();

        let snapshot = store.snapshot().await;
        assert_eq!(snapshot.nodes.len(), 2);
        assert_eq!(snapshot.edges.len(), 1);
    }

    #[tokio::test]
    async fn test_snapshot_at_time_filters_correctly() {
        let store = GraphStore::new();
        let mut early = make_node("n1", NodeType::Decision, "s1");
        early.timestamp = 100.0;
        let mut late = make_node("n2", NodeType::Decision, "s1");
        late.timestamp = 200.0;
        store.add_node(early).await;
        store.add_node(late).await;

        let snapshot = store.snapshot_at_time(150.0).await;
        assert_eq!(snapshot.nodes.len(), 1);
        assert_eq!(snapshot.nodes[0].id, "n1");
    }

    #[tokio::test]
    async fn test_bfs_subgraph_respects_depth() {
        let store = GraphStore::new();
        for i in 1..=5u32 {
            store.add_node(make_node(&format!("n{i}"), NodeType::Decision, "s1")).await;
        }
        // Chain: n1 → n2 → n3 → n4 → n5
        for i in 1..4u32 {
            store
                .add_edge(make_edge(
                    &format!("e{i}"),
                    EdgeType::RelatesTo,
                    &format!("n{i}"),
                    &format!("n{}", i + 1),
                ))
                .await
                .unwrap();
        }

        let subgraph = store.bfs_subgraph(&["n1".to_string()], 2).await;
        // Depth 2 from n1: n1, n2, n3 (not n4 or n5)
        assert_eq!(subgraph.nodes.len(), 3);
    }

    #[tokio::test]
    async fn test_debt_unresolved_open_item() {
        let store = GraphStore::new();
        store.add_node(make_node("q1", NodeType::OpenItem, "s1")).await;
        let report = store.compute_debt_report(0).await;
        let unresolved: Vec<_> = report
            .items
            .iter()
            .filter(|i| i.debt_type == "UnresolvedOpenItem")
            .collect();
        assert_eq!(unresolved.len(), 1);
        assert_eq!(unresolved[0].severity, "Medium");
    }

    #[tokio::test]
    async fn test_debt_unresolved_open_item_high_severity_across_session() {
        let store = GraphStore::new();
        store.add_node(make_node("q1", NodeType::OpenItem, "session-a")).await;
        store.add_node(make_node("d1", NodeType::Decision, "session-a")).await;
        // first session at index 0, current_session_idx = 1 means question is from prior session → age = 1
        let report = store.compute_debt_report(1).await;
        let item = report
            .items
            .iter()
            .find(|i| i.debt_type == "UnresolvedOpenItem")
            .unwrap();
        assert_eq!(item.severity, "High");
    }

    #[tokio::test]
    async fn test_debt_open_item_resolved_not_flagged() {
        let store = GraphStore::new();
        store.add_node(make_node("q1", NodeType::OpenItem, "s1")).await;
        store.add_node(make_node("d1", NodeType::Decision, "s1")).await;
        store
            .add_edge(make_edge("e1", EdgeType::Resolves, "d1", "q1"))
            .await
            .unwrap();
        let report = store.compute_debt_report(0).await;
        let unresolved: Vec<_> = report
            .items
            .iter()
            .filter(|i| i.debt_type == "UnresolvedOpenItem")
            .collect();
        assert!(unresolved.is_empty());
    }






    #[tokio::test]
    async fn test_debt_total_score() {
        let store = GraphStore::new();
        // High severity: unresolved question from prior meeting
        let mut q = make_node("q1", NodeType::OpenItem, "s1");
        q.timestamp = 0.0;
        store.add_node(q).await;
        // Low severity: orphan assumption
        store.add_node(make_node("a1", NodeType::OpenItem, "s1")).await;

        let report = store.compute_debt_report(1).await;
        // Both OpenItems are unresolved and aged → High (3) + High (3) = 6
        assert_eq!(report.total_score, 6);
    }

    #[tokio::test]
    async fn test_persistent_roundtrip() {
        let dir = std::env::temp_dir().join(format!("armin_graph_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);

        // Write phase
        {
            let store = GraphStore::new_persistent(&dir).unwrap();
            store.add_node(make_node("n1", NodeType::Decision, "s1")).await;
            store.add_node(make_node("n2", NodeType::Decision, "s1")).await;
            store.add_node(make_node("n3", NodeType::OpenItem, "s1")).await;
            store
                .add_edge(make_edge("e1", EdgeType::RelatesTo, "n2", "n1"))
                .await
                .unwrap();
            store
                .add_edge(make_edge("e2", EdgeType::RelatesTo, "n2", "n3"))
                .await
                .unwrap();
            assert_eq!(store.node_count().await, 3);
            assert_eq!(store.edge_count().await, 2);
            // Flush sled to disk
            let _ = store.flush().await;
        }

        // Read phase — simulate a crash/restart
        {
            let store = GraphStore::new_persistent(&dir).unwrap();
            assert_eq!(store.node_count().await, 3, "nodes survived restart");
            assert_eq!(store.edge_count().await, 2, "edges survived restart");

            let n1 = store.get_node("n1").await.unwrap();
            assert_eq!(n1.node_type, NodeType::Decision);
            assert_eq!(n1.label, "Label for n1");

            let snapshot = store.snapshot().await;
            assert_eq!(snapshot.nodes.len(), 3);
            assert_eq!(snapshot.edges.len(), 2);

            // last_n_nodes should work (order preserved)
            let last = store.last_n_nodes(2).await;
            assert_eq!(last.len(), 2);
            assert_eq!(last[0].id, "n2");
            assert_eq!(last[1].id, "n3");

            // Debt detection should work on reconstructed graph
            let report = store.compute_debt_report(0).await;
            assert!(report.items.iter().any(|i| i.debt_type == "UnresolvedOpenItem"));
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn test_persistent_empty_db() {
        let dir = std::env::temp_dir().join(format!("armin_graph_test_empty_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);

        {
            let store = GraphStore::new_persistent(&dir).unwrap();
            assert_eq!(store.node_count().await, 0);
            assert_eq!(store.edge_count().await, 0);
            let _ = store.flush().await;
        }

        {
            let store = GraphStore::new_persistent(&dir).unwrap();
            assert_eq!(store.node_count().await, 0);
            assert_eq!(store.edge_count().await, 0);
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn test_invalidate_node_clears_unresolved_open_item() {
        let store = GraphStore::new();
        store.add_node(make_node("a1", NodeType::OpenItem, "s1")).await;
        let report = store.compute_debt_report(0).await;
        assert!(report.items.iter().any(|i| i.debt_type == "UnresolvedOpenItem"));

        store.invalidate_node("a1").await.unwrap();
        let report = store.compute_debt_report(0).await;
        assert!(!report.items.iter().any(|i| i.debt_type == "UnresolvedOpenItem"));
    }

    #[tokio::test]
    async fn test_resolve_question_clears_unresolved_debt() {
        let store = GraphStore::new();
        store.add_node(make_node("q1", NodeType::OpenItem, "s1")).await;
        store.add_node(make_node("d1", NodeType::Decision, "s1")).await;

        let report = store.compute_debt_report(0).await;
        assert!(report.items.iter().any(|i| i.debt_type == "UnresolvedOpenItem"));

        store.resolve_question("q1", "d1", "Decision resolved the question").await.unwrap();
        let report = store.compute_debt_report(0).await;
        assert!(!report.items.iter().any(|i| i.debt_type == "UnresolvedOpenItem"));
    }

    #[tokio::test]
    async fn test_invalidate_persists_roundtrip() {
        let dir = std::env::temp_dir().join(format!("armin_graph_invalidate_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);

        // Write phase
        {
            let store = GraphStore::new_persistent(&dir).unwrap();
            store.add_node(make_node("a1", NodeType::OpenItem, "s1")).await;
            store.invalidate_node("a1").await.unwrap();
            let n = store.get_node("a1").await.unwrap();
            assert_eq!(n.status, NodeStatus::Invalidated);
            let _ = store.flush().await;
        }

        // Read phase — reload from disk
        {
            let store = GraphStore::new_persistent(&dir).unwrap();
            let n = store.get_node("a1").await.unwrap();
            assert_eq!(n.status, NodeStatus::Invalidated);

            // Debt detector should respect it
            let report = store.compute_debt_report(0).await;
            assert!(!report.items.iter().any(|i| i.debt_type == "UnresolvedOpenItem"));
        }

        let _ = std::fs::remove_dir_all(&dir);
    }
}
