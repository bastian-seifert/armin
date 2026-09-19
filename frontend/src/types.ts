export type NodeType = 'Rule' | 'Decision' | 'OpenItem'
export type EdgeType = 'Supersedes' | 'Refutes' | 'Resolves' | 'RelatesTo'

export type EdgeProvenance =
  | 'EXTRACTED'
  | { INFERRED: { confidence: number } }
  | 'AMBIGUOUS'

export interface ArgumentNode {
  id: string
  node_type: NodeType
  label: string
  description: string
  event_id: string
  agent_id: string
  session_id: string
  timestamp: number
  confidence: number
  files: string[]
  commit?: string | null
  mention_count: number
}

export interface ArgumentEdge {
  id: string
  edge_type: EdgeType
  source_node_id: string
  target_node_id: string
  reasoning: string
  timestamp: number
  evidence_score?: number | null
  provenance?: EdgeProvenance
}

export interface AgentEvent {
  id: string
  session_id: string
  agent_role: string
  start_time: number
  end_time: number
  text: string
}

export interface DebtItem {
  debt_type: string
  node_ids: string[]
  description: string
  severity: 'High' | 'Medium' | 'Low'
}

export interface DebtReport {
  items: DebtItem[]
  total_score: number
  timestamp: number
}

export interface QueryResult {
  answer: string
  trace: string[]
  cited_events: string[]
  mode_used: string
}

export interface Community {
  id: string
  label: string
  node_ids: string[]
  size: number
  top_node_types: [NodeType, number][]
  top_agents: [string, number][]
  session_distribution: Record<string, number>
}

export interface SurprisingConnection {
  source_node_id: string
  target_node_id: string
  edge_type: EdgeType
  source_community: string
  target_community: string
  unexpectedness: number
}

export interface CommunityReport {
  communities: Community[]
  timestamp: number
  modularity: number
  surprising_connections: SurprisingConnection[]
}

export type WsMessage =
  | {
      type: 'event'
      data: AgentEvent
      new_nodes: ArgumentNode[]
      new_edges: ArgumentEdge[]
      debt_report: DebtReport
      community_report: CommunityReport | null
    }
  | {
      type: 'session_boundary'
      session_id: string
      session_type: string
    }

export interface TransitionEntry {
  type: 'transition'
  session_id: string
  session_type: string
}

export type FeedEntry = AgentEvent | TransitionEntry
