import { createContext, useContext, useReducer, useEffect, useRef, useCallback } from 'react'
import type { ReactNode } from 'react'
import type {
  ArgumentEdge,
  ArgumentNode,
  CommunityReport,
  DebtReport,
  FeedEntry,
  TransitionEntry,
  AgentEvent,
  WsMessage,
} from '../types'

// ── State ──────────────────────────────────────────────────────────────────────

interface GraphState {
  nodes: Map<string, ArgumentNode>
  edges: Map<string, ArgumentEdge>
  feedEntries: FeedEntry[]
  debtReport: DebtReport | null
  communityReport: CommunityReport | null
  highlightedUtteranceIds: Set<string>
  traceNodeIds: Set<string>
  connected: boolean
  communityColorMode: boolean
}

const initialState: GraphState = {
  nodes: new Map(),
  edges: new Map(),
  feedEntries: [],
  debtReport: null,
  communityReport: null,
  highlightedUtteranceIds: new Set(),
  traceNodeIds: new Set(),
  connected: false,
  communityColorMode: false,
}

// ── Actions ────────────────────────────────────────────────────────────────────

type GraphAction =
  | { type: 'WS_CONNECTED' }
  | { type: 'WS_DISCONNECTED' }
  | { type: 'ADD_EVENT'; payload: { data: AgentEvent; new_nodes: ArgumentNode[]; new_edges: ArgumentEdge[]; debt_report: DebtReport; community_report: CommunityReport | null } }
  | { type: 'ADD_TRANSITION'; payload: TransitionEntry }
  | { type: 'SET_TRACE'; payload: Set<string> }
  | { type: 'SET_DEBT'; payload: DebtReport }
  | { type: 'SET_COMMUNITY_REPORT'; payload: CommunityReport | null }
  | { type: 'TOGGLE_COMMUNITY_COLOR' }

// ── Reducer ────────────────────────────────────────────────────────────────────

function graphReducer(state: GraphState, action: GraphAction): GraphState {
  switch (action.type) {
    case 'WS_CONNECTED':
      return { ...state, connected: true }

    case 'WS_DISCONNECTED':
      return { ...state, connected: false }

    case 'ADD_TRANSITION':
      return {
        ...state,
        feedEntries: [...state.feedEntries, action.payload],
      }

    case 'ADD_EVENT': {
      const { data, new_nodes, new_edges, debt_report, community_report } = action.payload

      // Deduplicate feed entries by ID
      const feedEntries = state.feedEntries.some(e => 'id' in e && e.id === data.id)
        ? state.feedEntries
        : [...state.feedEntries, data].slice(-500)

      const nodes = new Map(state.nodes)
      new_nodes.forEach(n => nodes.set(n.id, n))
      const hasNewNodes = new_nodes.length > 0

      const edges = new Map(state.edges)
      new_edges.forEach(e => edges.set(e.id, e))

      const highlightedUtteranceIds = hasNewNodes
        ? new Set([...state.highlightedUtteranceIds, data.id])
        : state.highlightedUtteranceIds

      return {
        ...state,
        feedEntries,
        nodes,
        edges,
        debtReport: debt_report,
        communityReport: community_report ?? state.communityReport,
        highlightedUtteranceIds,
      }
    }

    case 'SET_TRACE':
      return { ...state, traceNodeIds: action.payload }

    case 'SET_DEBT':
      return { ...state, debtReport: action.payload }

    case 'SET_COMMUNITY_REPORT':
      return { ...state, communityReport: action.payload }

    case 'TOGGLE_COMMUNITY_COLOR':
      return { ...state, communityColorMode: !state.communityColorMode }

    default:
      return state
  }
}

// ── Context ────────────────────────────────────────────────────────────────────

interface GraphContextValue {
  state: GraphState
  dispatch: React.Dispatch<GraphAction>
}

const GraphContext = createContext<GraphContextValue | null>(null)

// ── Provider ───────────────────────────────────────────────────────────────────

const WS_URL = import.meta.env.VITE_WS_URL ?? 'ws://localhost:8080/ws/stream'

interface GraphProviderProps {
  children: ReactNode
}

export function GraphProvider({ children }: GraphProviderProps) {
  const [state, dispatch] = useReducer(graphReducer, initialState)

  const wsRef = useRef<WebSocket | null>(null)
  const reconnectTimer = useRef<ReturnType<typeof setTimeout> | null>(null)
  const mountedRef = useRef(false)

  const connect = useCallback(() => {
    const ws = new WebSocket(WS_URL)
    wsRef.current = ws

    ws.onopen = () => dispatch({ type: 'WS_CONNECTED' })

    ws.onmessage = (event: MessageEvent<string>) => {
      const msg = JSON.parse(event.data) as WsMessage

      if (msg.type === 'session_boundary') {
        dispatch({
          type: 'ADD_TRANSITION',
          payload: {
            type: 'transition',
            session_id: msg.session_id,
            session_type: msg.session_type,
          },
        })
        return
      }

      dispatch({
        type: 'ADD_EVENT',
        payload: msg,
      })
    }

    ws.onclose = () => {
      dispatch({ type: 'WS_DISCONNECTED' })
      if (mountedRef.current) {
        reconnectTimer.current = setTimeout(connect, 3000)
      }
    }

    ws.onerror = () => ws.close()
  }, [])

  useEffect(() => {
    mountedRef.current = true
    connect()
    return () => {
      mountedRef.current = false
      if (reconnectTimer.current) clearTimeout(reconnectTimer.current)
      wsRef.current?.close()
    }
  }, [connect])

  return (
    <GraphContext.Provider value={{ state, dispatch }}>
      {children}
    </GraphContext.Provider>
  )
}

// ── Hook ───────────────────────────────────────────────────────────────────────

export function useGraph(): GraphContextValue {
  const ctx = useContext(GraphContext)
  if (!ctx) {
    throw new Error('useGraph must be used within a GraphProvider')
  }
  return ctx
}
