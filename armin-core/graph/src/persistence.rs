use std::path::Path;

use anyhow::{Context, Result};
use bincode::{deserialize, serialize};
use sled::{Db, Tree};

use crate::types::{ArgumentEdge, ArgumentNode};

const CF_NODES: &str = "nodes";
const CF_EDGES: &str = "edges";
const CF_META: &str = "meta";
const CF_ORDER: &str = "order";
const KEY_NODE_ORDER: &[u8] = b"node_order";
const KEY_SCHEMA_VERSION: &[u8] = b"schema_version";
const SCHEMA_VERSION: u32 = 1;

/// Persistent key-value backend for the argument graph.
///
/// Uses sled (pure-Rust embedded LSM-tree DB) as the storage engine.
/// Column families (sled Trees) provide namespace isolation:
/// - `nodes`: node_id → bincode(ArgumentNode)
/// - `edges`: edge_id → bincode(ArgumentEdge)
/// - `order`: insertion index (u64 big-endian) → node_id (O(1) appends)
/// - `meta`:  schema_version (+ legacy `node_order` blob for migration)
#[derive(Clone)]
pub struct DbBackend {
    db: Db,
    nodes: Tree,
    edges: Tree,
    order: Tree,
    meta: Tree,
}

impl DbBackend {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let db = sled::open(path.as_ref()).context("Failed to open sled database")?;
        let nodes = db
            .open_tree(CF_NODES)
            .context("Failed to open nodes tree")?;
        let edges = db
            .open_tree(CF_EDGES)
            .context("Failed to open edges tree")?;
        let order = db
            .open_tree(CF_ORDER)
            .context("Failed to open order tree")?;
        let meta = db
            .open_tree(CF_META)
            .context("Failed to open meta tree")?;
        Ok(Self { db, nodes, edges, order, meta })
    }

    pub fn load_all_nodes(&self) -> Result<Vec<ArgumentNode>> {
        self.nodes
            .iter()
            .map(|entry| {
                let (_, value) = entry?;
                deserialize(&value).context("Failed to deserialize ArgumentNode")
            })
            .collect()
    }

    pub fn load_all_edges(&self) -> Result<Vec<ArgumentEdge>> {
        self.edges
            .iter()
            .map(|entry| {
                let (_, value) = entry?;
                deserialize(&value).context("Failed to deserialize ArgumentEdge")
            })
            .collect()
    }

    /// Load the insertion-ordered node ID list.
    ///
    /// Reads the per-index `order` tree (O(1) per insert on write side);
    /// falls back to the legacy single-blob format and migrates it in place.
    pub fn load_node_order(&self) -> Result<Vec<String>> {
        let count = self.order.len();
        if count > 0 {
            let mut out = Vec::with_capacity(count);
            for entry in self.order.iter() {
                let (_, value) = entry?;
                let id: String = deserialize(&value).context("Failed to deserialize node order entry")?;
                out.push(id);
            }
            return Ok(out);
        }

        // Legacy format: one bincode Vec<String> under KEY_NODE_ORDER.
        let legacy: Option<Vec<String>> = self
            .meta
            .get(KEY_NODE_ORDER)?
            .map(|value| deserialize(&value).context("Failed to deserialize node_order"))
            .transpose()?;
        let legacy = legacy.unwrap_or_default();
        if !legacy.is_empty() {
            tracing::info!("Migrating node_order from legacy blob ({} nodes)", legacy.len());
            for (i, id) in legacy.iter().enumerate() {
                self.append_node_order_at(i as u64, id)?;
            }
            self.meta.remove(KEY_NODE_ORDER)?;
        }
        Ok(legacy)
    }

    /// Append a node ID to the insertion order log. O(1) — writes a single
    /// per-index key instead of re-serializing the whole list.
    pub fn append_node_order(&self, node_id: &str) -> Result<()> {
        let idx = self.order.len() as u64;
        self.append_node_order_at(idx, node_id)
    }

    fn append_node_order_at(&self, idx: u64, node_id: &str) -> Result<()> {
        let key = idx.to_be_bytes();
        self.order.insert(&key, serialize(node_id)?)?;
        if self.meta.get(KEY_SCHEMA_VERSION)?.is_none() {
            self.meta
                .insert(KEY_SCHEMA_VERSION, &SCHEMA_VERSION.to_le_bytes())?;
        }
        Ok(())
    }

    pub fn store_node(&self, node: &ArgumentNode) -> Result<()> {
        let bytes = serialize(node)?;
        self.nodes.insert(node.id.as_bytes(), bytes)?;
        Ok(())
    }

    pub fn store_edge(&self, edge: &ArgumentEdge) -> Result<()> {
        let bytes = serialize(edge)?;
        self.edges.insert(edge.id.as_bytes(), bytes)?;
        Ok(())
    }

    #[allow(dead_code)]
    pub fn remove_node(&self, id: &str) -> Result<()> {
        self.nodes.remove(id.as_bytes())?;
        Ok(())
    }

    #[allow(dead_code)]
    pub fn remove_edge(&self, id: &str) -> Result<()> {
        self.edges.remove(id.as_bytes())?;
        Ok(())
    }

    pub fn flush(&self) -> Result<()> {
        self.db.flush().context("Failed to flush sled database")?;
        Ok(())
    }
}
