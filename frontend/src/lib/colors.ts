import type { EdgeType, NodeType } from '../types'

export const NODE_COLORS: Record<NodeType, string> = {
  Rule: '#60a5fa',
  Decision: '#a78bfa',
  OpenItem: '#fbbf24',
}

export const EDGE_COLORS: Record<EdgeType, string> = {
  Supersedes: '#60a5fa',
  Refutes: '#f87171',
  Resolves: '#a78bfa',
  RelatesTo: '#34d399',
}

export const NODE_GLOW: Record<NodeType, string> = {
  Rule: 'rgba(96,165,250,0.3)',
  Decision: 'rgba(167,139,250,0.3)',
  OpenItem: 'rgba(251,191,36,0.3)',
}

export const AGENT_COLORS: Record<string, string> = {
  user: 'bg-blue-900/60 border-blue-700',
  assistant: 'bg-emerald-900/60 border-emerald-700',
  agent: 'bg-emerald-900/60 border-emerald-700',
}

export const AGENT_LABELS: Record<string, string> = {
  user: 'User',
  assistant: 'Agent',
  agent: 'Agent',
}

export const COMMUNITY_COLORS: string[] = [
  '#6366f1', '#8b5cf6', '#a855f7', '#d946ef',
  '#ec4899', '#f43f5e', '#f97316', '#eab308',
  '#84cc16', '#22d3ee', '#06b6d4', '#3b82f6',
]

export function hexAlpha(hex: string, alpha: number): string {
  const r = parseInt(hex.slice(1, 3), 16)
  const g = parseInt(hex.slice(3, 5), 16)
  const b = parseInt(hex.slice(5, 7), 16)
  return `rgba(${r},${g},${b},${alpha})`
}

export function provenanceDashes(provenance: unknown): number[] {
  if (provenance === 'EXTRACTED') return [1, 0]
  if (typeof provenance === 'object' && provenance !== null && 'INFERRED' in provenance) return [6, 3]
  return [2, 4]
}
