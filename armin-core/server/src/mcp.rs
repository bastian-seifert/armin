use std::sync::Arc;

use rmcp::handler::server::ServerHandler;
use rmcp::model::*;
use rmcp::service::{RequestContext, RoleServer};
use serde_json::Value;

use crate::state::AppState;

fn input_schema(schema: Value) -> Arc<JsonObject> {
    Arc::new(serde_json::from_value(schema).expect("valid input schema"))
}

#[derive(Clone)]
pub struct ArminMcpHandler {
    state: AppState,
}

impl ArminMcpHandler {
    pub fn new(state: AppState) -> Self {
        Self { state }
    }
}

impl ServerHandler for ArminMcpHandler {
    fn get_info(&self) -> ServerInfo {
        let mut info = ServerInfo::default();
        info.server_info = Implementation::new("armin-mcp", "0.1.0");
        let mut caps = ServerCapabilities::default();
        let mut tc = ToolsCapability::default();
        tc.list_changed = Some(true);
        caps.tools = Some(tc);
        info.capabilities = caps;
        info
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        let tools = vec![
            Tool::new(
                "query_graph",
                "Ask a question about the argument graph.",
                input_schema(serde_json::json!({
                    "type": "object",
                    "properties": {
                        "question": {
                            "type": "string",
                            "description": "The question to ask about the argument graph"
                        },
                        "mode": {
                            "type": "string",
                            "enum": ["deterministic", "llm", "embedding", "hybrid"],
                            "description": "Query mode: deterministic, llm, embedding, or hybrid"
                        },
                        "depth": {
                            "type": "integer",
                            "description": "Graph traversal depth (default: 3)"
                        }
                    },
                    "required": ["question"]
                })),
            ),
            Tool::new(
                "record_decision",
                "Record a decision you have made.",
                input_schema(serde_json::json!({
                    "type": "object",
                    "properties": {
                        "label": { "type": "string", "description": "Short summary of the decision (max ~15 words)" },
                        "description": { "type": "string", "description": "Detailed explanation of the decision" },
                        "session_id": { "type": "string", "description": "Session identifier" },
                        "resolves": { "type": "array", "items": { "type": "string" }, "description": "IDs of questions this decision resolves" },
                        "resolutions": { "type": "array", "items": { "type": "string" }, "description": "Reasoning for each resolution" },
                        "files": { "type": "array", "items": { "type": "string" }, "description": "File paths" },
                        "commit": { "type": "string", "description": "Commit hash" }
                    },
                    "required": ["label", "description", "session_id"]
                })),
            ),
            Tool::new(
                "raise_question",
                "Record an unresolved question.",
                input_schema(serde_json::json!({
                    "type": "object",
                    "properties": {
                        "label": { "type": "string", "description": "Short summary of the question (max ~15 words)" },
                        "description": { "type": "string", "description": "Detailed explanation" },
                        "session_id": { "type": "string", "description": "Session identifier" },
                        "files": { "type": "array", "items": { "type": "string" }, "description": "File paths" }
                    },
                    "required": ["label", "description", "session_id"]
                })),
            ),
            Tool::new(
                "resolve_question",
                "Mark a question as resolved by linking it to a node.",
                input_schema(serde_json::json!({
                    "type": "object",
                    "properties": {
                        "question_id": { "type": "string", "description": "ID of the question node to resolve" },
                        "resolver_node_id": { "type": "string", "description": "ID of the resolving node" },
                        "reasoning": { "type": "string", "description": "Explanation of how the resolver answers the question" }
                    },
                    "required": ["question_id", "resolver_node_id", "reasoning"]
                })),
            ),
            Tool::new(
                "invalidate_assumption",
                "Mark a previously recorded assumption as no longer valid.",
                input_schema(serde_json::json!({
                    "type": "object",
                    "properties": {
                        "node_id": { "type": "string", "description": "ID of the assumption node to invalidate" },
                        "rationale": { "type": "string", "description": "Why the assumption no longer holds" }
                    },
                    "required": ["node_id", "rationale"]
                })),
            ),
            Tool::new(
                "get_decisions",
                "List all extracted decisions with their status.",
                input_schema(serde_json::json!({ "type": "object", "properties": {} })),
            ),
            Tool::new(
                "get_risks",
                "List all detected risks with impact scores.",
                input_schema(serde_json::json!({ "type": "object", "properties": {} })),
            ),
            Tool::new(
                "get_debt",
                "Get the reasoning debt report.",
                input_schema(serde_json::json!({ "type": "object", "properties": {} })),
            ),
            Tool::new(
                "get_summary",
                "Get the executive summary.",
                input_schema(serde_json::json!({ "type": "object", "properties": {} })),
            ),
            Tool::new(
                "get_community",
                "Get the community detection report.",
                input_schema(serde_json::json!({ "type": "object", "properties": {} })),
            ),
        ];
        Ok(ListToolsResult::with_all_items(tools))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        let name = request.name.as_ref();
        let args = Value::Object(request.arguments.unwrap_or_default());
        match name {
            "query_graph" => self.handle_query_graph(args).await,
            "record_decision" => self.handle_record_decision(args).await,
            "raise_question" => self.handle_raise_question(args).await,
            "resolve_question" => self.handle_resolve_question(args).await,
            "invalidate_assumption" => self.handle_invalidate_assumption(args).await,
            "get_decisions" => self.handle_get_decisions().await,
            "get_risks" => self.handle_get_risks().await,
            "get_debt" => self.handle_get_debt().await,
            "get_summary" => self.handle_get_summary().await,
            "get_community" => self.handle_get_community().await,
            name => Err(ErrorData::invalid_request(format!("Unknown tool: {name}"), None)),
        }
    }
}

impl ArminMcpHandler {
    fn text_result(body: String) -> CallToolResult {
        CallToolResult::success(vec![ContentBlock::text(body)])
    }

    fn json_result(value: &impl serde::Serialize) -> CallToolResult {
        match serde_json::to_string_pretty(value) {
            Ok(body) => Self::text_result(body),
            Err(e) => CallToolResult::error(vec![ContentBlock::text(format!("Serialization error: {e}"))]),
        }
    }

    fn error(msg: impl Into<String>) -> CallToolResult {
        CallToolResult::error(vec![ContentBlock::text(msg.into())])
    }

    async fn handle_query_graph(&self, args: Value) -> Result<CallToolResult, ErrorData> {
        let question = args["question"].as_str().ok_or_else(|| {
            ErrorData::invalid_params("Missing 'question'", None)
        })?;
        let mode = args["mode"].as_str().unwrap_or("deterministic");
        let depth = args["depth"].as_u64().unwrap_or(3) as usize;

        let question = question.to_string();
        let seed_ids = self.state.retriever.find_relevant_nodes(&question, 5).await;
        let subgraph = self.state.graph.bfs_subgraph(&seed_ids, depth).await;

        let result = if mode == "llm" {
            self.state.extractor.answer_query(&question, &subgraph).await
                .unwrap_or(armin_graph::QueryResult {
                    answer: "Query failed".to_string(),
                    trace: vec![],
                    cited_events: vec![],
                    mode_used: "llm".to_string(),
                })
        } else {
            let mut result = self.state.graph.query_subgraph(&question, &subgraph).await;
            result.answer = crate::answer::build_template_answer(&result.trace, &subgraph);
            result
        };

        Ok(Self::json_result(&result))
    }

    async fn handle_record_decision(&self, args: Value) -> Result<CallToolResult, ErrorData> {
        let req: armin_graph::AgentDecisionRequest = serde_json::from_value(args)
            .map_err(|e| ErrorData::invalid_params(format!("Invalid arguments: {e}"), None))?;

        let node_id = uuid::Uuid::new_v4().to_string();
        let event_id = uuid::Uuid::new_v4().to_string();
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();

        let node = armin_graph::ArgumentNode {
            id: node_id.clone(),
            node_type: armin_graph::NodeType::Decision,
            label: req.label,
            description: req.description,
            event_id,
            agent_id: req.agent_id.unwrap_or_else(|| "agent".to_string()),
            session_id: req.session_id,
            timestamp: ts,
            confidence: 1.0,
            files: req.files,
            commit: req.commit,
            mention_count: 1,
            status: armin_graph::NodeStatus::Active,
        };

        self.state.graph.add_node(node.clone()).await;

        let mut edge_ids = Vec::new();
        for (i, target_id) in req.resolves.iter().enumerate() {
            let reasoning = req.resolutions.get(i).cloned().unwrap_or_else(|| {
                format!("Decision '{}' resolves this", node.label)
            });
            if let Ok(edge) = self.state.graph.resolve_question(target_id, &node_id, &reasoning).await {
                edge_ids.push(edge.id);
            }
        }

        Ok(Self::json_result(&armin_graph::AgentWriteResponse { node_id, node_label: node.label, edge_ids }))
    }

    async fn handle_raise_question(&self, args: Value) -> Result<CallToolResult, ErrorData> {
        let req: armin_graph::AgentQuestionRequest = serde_json::from_value(args)
            .map_err(|e| ErrorData::invalid_params(format!("Invalid arguments: {e}"), None))?;

        let node_id = uuid::Uuid::new_v4().to_string();
        let event_id = uuid::Uuid::new_v4().to_string();
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();

        let node = armin_graph::ArgumentNode {
            id: node_id.clone(),
            node_type: armin_graph::NodeType::OpenItem,
            label: req.label,
            description: req.description,
            event_id,
            agent_id: req.agent_id.unwrap_or_else(|| "agent".to_string()),
            session_id: req.session_id,
            timestamp: ts,
            confidence: 1.0,
            files: req.files,
            commit: req.commit,
            mention_count: 1,
            status: armin_graph::NodeStatus::Active,
        };

        self.state.graph.add_node(node.clone()).await;

        Ok(Self::json_result(&armin_graph::AgentWriteResponse { node_id, node_label: node.label, edge_ids: vec![] }))
    }

    async fn handle_resolve_question(&self, args: Value) -> Result<CallToolResult, ErrorData> {
        let question_id = args["question_id"].as_str().ok_or_else(|| {
            ErrorData::invalid_params("Missing 'question_id'", None)
        })?;
        let resolver_node_id = args["resolver_node_id"].as_str().ok_or_else(|| {
            ErrorData::invalid_params("Missing 'resolver_node_id'", None)
        })?;
        let reasoning = args["reasoning"].as_str().unwrap_or("Resolved via MCP");

        match self.state.graph.resolve_question(question_id, resolver_node_id, reasoning).await {
            Ok(edge) => Ok(Self::json_result(&armin_graph::AgentWriteResponse {
                node_id: question_id.to_string(),
                node_label: "question_resolved".to_string(),
                edge_ids: vec![edge.id],
            })),
            Err(e) => Ok(Self::error(format!("Resolution failed: {e}"))),
        }
    }

    async fn handle_invalidate_assumption(&self, args: Value) -> Result<CallToolResult, ErrorData> {
        let node_id = args["node_id"].as_str().ok_or_else(|| {
            ErrorData::invalid_params("Missing 'node_id'", None)
        })?;
        let rationale = args["rationale"].as_str().unwrap_or("Invalidated via MCP");

        match self.state.graph.invalidate_node(node_id).await {
            Ok(()) => {
                let claim_id = uuid::Uuid::new_v4().to_string();
                let claim = armin_graph::ArgumentNode {
                    id: claim_id.clone(),
                    node_type: armin_graph::NodeType::Decision,
                    label: format!("Invalidated: {}", rationale.chars().take(60).collect::<String>()),
                    description: rationale.to_string(),
                    event_id: uuid::Uuid::new_v4().to_string(),
                    agent_id: "agent".to_string(),
                    session_id: String::new(),
                    timestamp: std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs_f64(),
                    confidence: 1.0,
                    files: vec![],
                    commit: None,
                    mention_count: 1,
                    status: armin_graph::NodeStatus::Active,
                };
                self.state.graph.add_node(claim.clone()).await;

                let edge = armin_graph::ArgumentEdge {
                    id: uuid::Uuid::new_v4().to_string(),
                    edge_type: armin_graph::EdgeType::Refutes,
                    source_node_id: claim_id.clone(),
                    target_node_id: node_id.to_string(),
                    reasoning: format!("Invalidated: {}", claim.description),
                    timestamp: std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs_f64(),
                    evidence_score: None,
                    provenance: armin_graph::EdgeProvenance::Extracted,
                };
                let _ = self.state.graph.add_edge(edge.clone()).await;

                Ok(Self::json_result(&armin_graph::AgentWriteResponse {
                    node_id: node_id.to_string(),
                    node_label: "invalidated".to_string(),
                    edge_ids: vec![edge.id],
                }))
            }
            Err(e) => Ok(Self::error(format!("Invalidation failed: {e}"))),
        }
    }

    async fn handle_get_decisions(&self) -> Result<CallToolResult, ErrorData> {
        let decisions = self.state.graph.extract_decisions(
            self.state.current_session_idx.load(std::sync::atomic::Ordering::SeqCst)
        ).await;
        Ok(Self::json_result(&decisions))
    }

    async fn handle_get_risks(&self) -> Result<CallToolResult, ErrorData> {
        let risks = self.state.graph.compute_risks(
            self.state.current_session_idx.load(std::sync::atomic::Ordering::SeqCst)
        ).await;
        Ok(Self::json_result(&risks))
    }

    async fn handle_get_debt(&self) -> Result<CallToolResult, ErrorData> {
        let debt = self.state.graph.compute_debt_report(
            self.state.current_session_idx.load(std::sync::atomic::Ordering::SeqCst)
        ).await;
        Ok(Self::json_result(&debt))
    }

    async fn handle_get_summary(&self) -> Result<CallToolResult, ErrorData> {
        let session_idx = self.state.current_session_idx.load(std::sync::atomic::Ordering::SeqCst);
        let prior_debt = self.state.prior_debt_report.read().await.clone();
        let summary = self.state.graph.compute_summary(session_idx, prior_debt.as_ref()).await;
        Ok(Self::json_result(&summary))
    }

    async fn handle_get_community(&self) -> Result<CallToolResult, ErrorData> {
        let report = self.state.graph.compute_community_report().await;
        Ok(Self::json_result(&report))
    }
}
