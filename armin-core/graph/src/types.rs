use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// Durable knowledge node types (v2 taxonomy).
///
/// The graph holds only knowledge that future sessions need to keep the
/// codebase consistent. Episodic session reasoning is pipeline scratch, not
/// graph content. The measured-minimum set (backtest + abtest evidence):
///
/// - `Decision` — was this decided before, and why? (the abtest payload)
/// - `OpenItem` — what is known broken, unfinished, or unresolved?
///
/// `Rule` (binding conventions, constraints, requirements) was re-added for
/// deterministic paths — AGENTS.md/CLAUDE.md import and scoped-brief
/// injection. It is NOT extracted from prose by jev (criteria untouched);
/// prose-level Rule extraction is gated on a v2 gold set.
///
/// Future extensions, deliberately not extracted yet (see data/backtest
/// evaluation: Fact needs a durability gate, Lesson never fires):
/// `Fact` (verified gotchas), `Lesson` (tried-and-failed). Add the variant,
/// the extraction criterion, and a debt detector together.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "PascalCase")]
pub enum NodeType {
    Rule,
    Decision,
    OpenItem,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "PascalCase")]
#[derive(Default)]
pub enum NodeStatus {
    #[default]
    Active,
    Invalidated,
}


#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "PascalCase")]
pub enum EdgeType {
    /// The source item replaces or overrides the target (decision/rule).
    Supersedes,
    /// The source shows the target fact/rule is wrong or no longer holds.
    Refutes,
    /// The source settles or answers the target open item.
    Resolves,
    /// The items share a concern — one constrains, grounds, or explains the other.
    RelatesTo,
}

impl EdgeType {
    pub fn preference_order(&self) -> u8 {
        match self {
            EdgeType::RelatesTo => 0,
            EdgeType::Resolves => 1,
            EdgeType::Refutes => 2,
            EdgeType::Supersedes => 3,
        }
    }

    pub fn community_weight(&self) -> f64 {
        match self {
            EdgeType::RelatesTo => 1.0,
            EdgeType::Resolves => 0.8,
            EdgeType::Supersedes => 0.6,
            EdgeType::Refutes => 0.3,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ArgumentNode {
    pub id: String,
    pub node_type: NodeType,
    /// Short natural language summary, max ~15 words
    pub label: String,
    pub description: String,
    pub event_id: String,
    pub agent_id: String,
    pub session_id: String,
    pub timestamp: f64,
    pub confidence: f32,
    /// File paths referenced by the originating event.
    #[serde(default)]
    pub files: Vec<String>,
    /// Commit hash referenced by the originating event.
    #[serde(default)]
    pub commit: Option<String>,
    /// Number of distinct events that resolved to this node via dedup.
    #[serde(default)]
    pub mention_count: u32,
    /// Whether this node is still active or has been invalidated.
    #[serde(default)]
    pub status: NodeStatus,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum EdgeProvenance {
    /// Explicitly stated in the transcript (e.g., "X supports Y because...").
    Extracted,
    /// Reasonably deduced by the LLM from context across events.
    Inferred { confidence: f32 },
    /// Uncertain — the LLM flagged this as speculative; needs human review.
    Ambiguous,
}

impl Default for EdgeProvenance {
    fn default() -> Self {
        Self::Inferred { confidence: 0.5 }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ArgumentEdge {
    pub id: String,
    pub edge_type: EdgeType,
    pub source_node_id: String,
    pub target_node_id: String,
    pub reasoning: String,
    pub timestamp: f64,
    /// Grounding evidence score in [0, 1], None when not yet scored.
    #[serde(default)]
    pub evidence_score: Option<f64>,
    /// How directly this connection was found in the transcript vs. deduced.
    #[serde(default)]
    pub provenance: EdgeProvenance,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct ExtractionResult {
    pub new_nodes: Vec<ArgumentNode>,
    pub new_edges: Vec<ArgumentEdge>,
}

// ── Session scratch (episodic, never graph content) ───────────────────────────

/// One mutating tool call (edit/write/...), recorded in session scratch.
/// Feeds the cross-layer `UnverifiedChange` detector.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ScratchEdit {
    pub event_id: String,
    pub tool: String,
    pub files: Vec<String>,
    pub timestamp: f64,
}

/// One verification tool call (test/lint/build) with its outcome.
/// Feeds the `FailedVerification` and `RuleViolation` detectors.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ScratchCheck {
    pub event_id: String,
    pub tool: String,
    pub files: Vec<String>,
    pub passed: bool,
    pub timestamp: f64,
}

/// The scratch slice the debt detectors need, built per session by the
/// engine. The graph stays durable-only; this is passed IN, never stored.
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct ScratchSnapshot {
    pub edits: Vec<ScratchEdit>,
    pub checks: Vec<ScratchCheck>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DebtItem {
    pub debt_type: String,
    pub node_ids: Vec<String>,
    pub description: String,
    pub severity: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DebtReport {
    pub items: Vec<DebtItem>,
    pub total_score: u32,
    pub timestamp: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct QueryResult {
    pub answer: String,
    pub trace: Vec<String>,
    pub cited_events: Vec<String>,
    pub mode_used: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GraphSnapshot {
    pub nodes: Vec<ArgumentNode>,
    pub edges: Vec<ArgumentEdge>,
}

// ── Community Detection Types ─────────────────────────────────────────────────

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Community {
    pub id: String,
    pub label: String,
    pub node_ids: Vec<String>,
    pub size: usize,
    pub top_node_types: Vec<(NodeType, usize)>,
    pub top_agents: Vec<(String, usize)>,
    pub session_distribution: HashMap<String, usize>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SurprisingConnection {
    pub source_node_id: String,
    pub target_node_id: String,
    pub edge_type: EdgeType,
    pub source_community: String,
    pub target_community: String,
    pub unexpectedness: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CommunityReport {
    pub communities: Vec<Community>,
    pub timestamp: f64,
    pub modularity: f64,
    pub surprising_connections: Vec<SurprisingConnection>,
}

// ── Executive Summary Types ────────────────────────────────────────────────────

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Decision {
    pub id: String,
    pub label: String,
    pub rationale: String,
    pub status: String,
    pub owner: Option<String>,
    pub session_id: String,
    pub timestamp: f64,
    pub blocked_work: Vec<String>,
    pub validation_criteria: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Risk {
    pub node_id: String,
    pub label: String,
    pub impact_score: f32,
    pub validation_status: String,
    pub debt_type: String,
    pub session_id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct DebtDelta {
    pub new: u32,
    pub resolved: u32,
    pub persisted: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExecutiveSummary {
    pub decisions: Vec<Decision>,
    pub risks: Vec<Risk>,
    pub debt_delta: DebtDelta,
    pub session_id: String,
    pub prior_session_id: Option<String>,
    pub generated_at: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GraphDiff {
    pub added: GraphSnapshot,
    pub removed: GraphSnapshot,
    pub changed: Vec<ChangedNode>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChangedNode {
    pub node_id: String,
    pub fields: Vec<String>,
}

// ── Agent Write Types ─────────────────────────────────────────────────────────

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AgentDecisionRequest {
    pub label: String,
    pub description: String,
    pub session_id: String,
    /// IDs of nodes this decision resolves (Questions, Contradictions, etc.)
    #[serde(default)]
    pub resolves: Vec<String>,
    /// Rationale for each resolution edge
    #[serde(default)]
    pub resolutions: Vec<String>,
    /// File paths the decision relates to
    #[serde(default)]
    pub files: Vec<String>,
    /// Commit hash if known
    #[serde(default)]
    pub commit: Option<String>,
    #[serde(default)]
    pub agent_id: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AgentQuestionRequest {
    pub label: String,
    pub description: String,
    pub session_id: String,
    #[serde(default)]
    pub files: Vec<String>,
    #[serde(default)]
    pub commit: Option<String>,
    #[serde(default)]
    pub agent_id: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AgentResolveRequest {
    pub question_id: String,
    pub resolver_node_id: String,
    pub reasoning: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AgentInvalidateRequest {
    pub node_id: String,
    pub rationale: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AgentWriteResponse {
    pub node_id: String,
    pub node_label: String,
    #[serde(default)]
    pub edge_ids: Vec<String>,
}
