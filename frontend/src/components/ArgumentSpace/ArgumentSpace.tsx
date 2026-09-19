import { AnimatePresence } from 'motion/react'
import { forwardRef, useCallback, useEffect, useImperativeHandle, useMemo, useRef, useState } from 'react'
import type { ArgumentNode } from '../../types'
import { useGraph } from '../../context/GraphContext'
import { COMMUNITY_COLORS, NODE_COLORS } from '../../lib/colors'
import { useTemporalGraph } from '../../hooks/useTemporalGraph'
import { CommunityLegend } from './CommunityLegend'
import { NodeDetail } from './NodeDetail'
import { GraphControls } from './GraphControls'

interface Props {
  traceNodeIds?: Set<string>
}

export interface ArgumentSpaceHandle {
  focusOnNode: (id: string) => void
}

export const ArgumentSpace = forwardRef<ArgumentSpaceHandle, Props>(function ArgumentSpace(
  { traceNodeIds = new Set() },
  ref,
) {
  const { state, dispatch } = useGraph()
  const { nodes, edges, communityReport, communityColorMode } = state
  const [selectedNode, setSelectedNode] = useState<ArgumentNode | null>(null)
  const [cursor, setCursor] = useState<'grab' | 'pointer' | 'grabbing'>('grab')
  const [tooltip, setTooltip] = useState<{ x: number; y: number; node: ArgumentNode } | null>(null)
  const pointerDownPos = useRef<{ x: number; y: number } | null>(null)
  const isPanningRef = useRef(false)
  const hoverRafRef = useRef<number>(0)
  const lastMouseRef = useRef<{ x: number; y: number } | null>(null)

  const communityMap = useMemo(() => {
    if (!communityReport) return undefined
    const map = new Map<string, number>()
    for (const c of communityReport.communities) {
      const id = parseInt(c.id)
      for (const nid of c.node_ids) {
        map.set(nid, id)
      }
    }
    return map
  }, [communityReport])

  const { canvasRef, getNodeAtPoint, getSessionAtPoint, controls, transform } = useTemporalGraph(
    nodes,
    edges,
    traceNodeIds,
    selectedNode?.id ?? null,
    communityMap,
    communityColorMode,
  )

  useImperativeHandle(
    ref,
    () => ({
      focusOnNode: (id: string) => {
        controls.focusOnNode(id)
      },
    }),
    [controls],
  )

  const updateHover = useCallback(() => {
    const m = lastMouseRef.current
    if (!m) return
    if (isPanningRef.current) {
      setTooltip(null)
      return
    }
    const node = getNodeAtPoint(m.x, m.y)
    setTooltip(node ? { x: m.x, y: m.y, node } : null)
    setCursor(node ? 'pointer' : 'grab')
  }, [getNodeAtPoint])

  const handlePointerDown = (e: React.PointerEvent<HTMLCanvasElement>) => {
    pointerDownPos.current = { x: e.clientX, y: e.clientY }
    isPanningRef.current = true
    setCursor('grabbing')
  }

  const handlePointerUp = (e: React.PointerEvent<HTMLCanvasElement>) => {
    isPanningRef.current = false
    const start = pointerDownPos.current
    pointerDownPos.current = null
    const rect = canvasRef.current?.getBoundingClientRect()
    if (!rect) return
    const mx = e.clientX - rect.left
    const my = e.clientY - rect.top
    lastMouseRef.current = { x: mx, y: my }
    const node = getNodeAtPoint(mx, my)
    setTooltip(node ? { x: mx, y: my, node } : null)
    setCursor(node ? 'pointer' : 'grab')
    if (start) {
      const moved = Math.hypot(e.clientX - start.x, e.clientY - start.y)
      if (moved >= 5) return
    }
    if (node) {
      setSelectedNode(node)
      return
    }
    const session = getSessionAtPoint(mx, my)
    if (session) {
      controls.focusOnSession(session)
      return
    }
    setSelectedNode(null)
  }

  const handlePointerMove = (e: React.PointerEvent<HTMLCanvasElement>) => {
    const rect = canvasRef.current?.getBoundingClientRect()
    if (!rect) return
    const mx = e.clientX - rect.left
    const my = e.clientY - rect.top
    lastMouseRef.current = { x: mx, y: my }
    if (hoverRafRef.current) return
    hoverRafRef.current = requestAnimationFrame(() => {
      hoverRafRef.current = 0
      updateHover()
    })
  }

  const handlePointerLeave = () => {
    lastMouseRef.current = null
    setTooltip(null)
    isPanningRef.current = false
    setCursor('grab')
  }

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const target = e.target as HTMLElement | null
      if (target) {
        const tag = target.tagName
        if (tag === 'INPUT' || tag === 'TEXTAREA' || target.isContentEditable) return
      }
      switch (e.key) {
        case '+':
        case '=':
          controls.zoomIn()
          e.preventDefault()
          break
        case '-':
        case '_':
          controls.zoomOut()
          e.preventDefault()
          break
        case '0':
          controls.reset()
          e.preventDefault()
          break
        case 'f':
        case 'F':
          controls.fit()
          e.preventDefault()
          break
        case 'Escape':
          setSelectedNode(null)
          break
      }
    }
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [controls])

  const tooltipStyle = useMemo(() => {
    if (!tooltip) return null
    const W = canvasRef.current?.clientWidth ?? 0
    const tooltipW = 220
    const left = tooltip.x + 14 + tooltipW > W ? tooltip.x - tooltipW - 14 : tooltip.x + 14
    return { left, top: tooltip.y + 14 }
  }, [tooltip])

  return (
    <div className="relative h-full w-full flex flex-col">
      <CommunityLegend
        communities={communityReport?.communities ?? []}
        onToggleColorMode={() => dispatch({ type: 'TOGGLE_COMMUNITY_COLOR' })}
        communityColorMode={communityColorMode}
      />
      <div className="relative flex-1 min-h-0">
        <canvas
          ref={canvasRef}
          className="w-full h-full"
          style={{ display: 'block', cursor }}
          onPointerDown={handlePointerDown}
          onPointerUp={handlePointerUp}
          onPointerMove={handlePointerMove}
          onPointerLeave={handlePointerLeave}
        />

        {tooltip && tooltipStyle && (
          <div
            className="absolute pointer-events-none z-30 bg-gray-900/95 border border-gray-700 rounded-lg px-3 py-2 text-xs text-gray-200 shadow-xl max-w-[220px]"
            style={tooltipStyle}
          >
            <div className="font-semibold text-white mb-1 break-words">{tooltip.node.label}</div>
            <div className="flex items-center gap-1.5 text-gray-400">
              <span
                className="inline-block w-1.5 h-1.5 rounded-full"
                style={{
                  backgroundColor: communityColorMode && communityMap
                    ? COMMUNITY_COLORS[(communityMap.get(tooltip.node.id) ?? 0) % COMMUNITY_COLORS.length]
                    : NODE_COLORS[tooltip.node.node_type],
                }}
              />
              <span>{tooltip.node.node_type}</span>
              <span className="text-gray-600">·</span>
              <span>{tooltip.node.agent_id}</span>
              <span className="text-gray-600">·</span>
              <span>{tooltip.node.session_id}</span>
            </div>
          </div>
        )}

        <div className="absolute top-3 right-3 z-10 bg-gray-900/80 border border-gray-700 rounded-lg px-3 py-1.5 text-xs flex items-center gap-3">
          {(Object.entries(NODE_COLORS) as [ArgumentNode['node_type'], string][]).map(([type, color]) => (
            <div key={type} className="flex items-center gap-1">
              <div className="w-2 h-2 rounded-full" style={{ backgroundColor: color }} />
              <span className="text-gray-400">{type}</span>
            </div>
          ))}
          <span className="text-gray-700">|</span>
          <span className="text-gray-400 whitespace-nowrap">
            {nodes.size} nodes · {edges.size} edges
          </span>
        </div>

        <GraphControls controls={controls} k={transform.k} />

        <AnimatePresence>
          {selectedNode && (
            <NodeDetail
              key={selectedNode.id}
              node={selectedNode}
              onClose={() => setSelectedNode(null)}
              communityReport={communityReport}
            />
          )}
        </AnimatePresence>
      </div>
    </div>
  )
})
