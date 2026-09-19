import * as d3zoom from 'd3-zoom'
import * as d3selection from 'd3-selection'
import 'd3-transition'
import { type RefObject, useCallback, useEffect, useMemo, useRef, useState } from 'react'
import type { ArgumentEdge, ArgumentNode, NodeType } from '../types'
import { COMMUNITY_COLORS, EDGE_COLORS, NODE_COLORS, NODE_GLOW, hexAlpha, provenanceDashes } from '../lib/colors'
import {
  clampedFont,
  clampedStroke,
  clampedDash,
  fitTransform,
  makeSpring,
  type BBox,
} from '../lib/zoom'

const LANE_ORDER: NodeType[] = ['Decision', 'Rule', 'OpenItem']
const MARGIN = { top: 50, right: 30, bottom: 20, left: 90 }
const NODE_R = 8
const STAGGER = NODE_R * 2 + 4
const HIT_RADIUS_SCREEN_PX = 14
const MAX_ARC_SCREEN_PX = 120
const DIM_LERP = 0.18

const DIM_NODE_COLOR = 'rgba(60,60,70,0.4)'
const DIM_EDGE_COLOR = 'rgba(100,100,100,0.2)'

interface Pos { x: number; y: number }
interface Spring { x: ReturnType<typeof makeSpring>; y: ReturnType<typeof makeSpring> }

export interface GraphControls {
  zoomIn(): void
  zoomOut(): void
  fit(): void
  reset(): void
  focusOnNode(id: string): void
  focusOnSession(sessionId: string): void
}

interface UseTemporalGraphResult {
  canvasRef: RefObject<HTMLCanvasElement>
  getNodeAtPoint: (mx: number, my: number) => ArgumentNode | null
  getSessionAtPoint: (mx: number, my: number) => string | null
  controls: GraphControls
  transform: d3zoom.ZoomTransform
}

function computePositions(
  nodes: Map<string, ArgumentNode>,
  contentW: number,
  contentH: number,
): Map<string, Pos> {
  const nodeArr = Array.from(nodes.values())
  if (nodeArr.length === 0) return new Map()

  const tMin = Math.min(...nodeArr.map(n => n.timestamp))
  const tMax = Math.max(...nodeArr.map(n => n.timestamp))
  const tRange = tMax - tMin || 1
  const laneH = contentH / LANE_ORDER.length
  const xPad = NODE_R * 3

  const baseX = new Map<string, number>()
  for (const node of nodeArr) {
    baseX.set(node.id, ((node.timestamp - tMin) / tRange) * (contentW - xPad * 2) + xPad)
  }

  const buckets = new Map<string, string[]>()
  for (const node of nodeArr) {
    const laneIdx = LANE_ORDER.indexOf(node.node_type)
    const bx = Math.floor(baseX.get(node.id)! / (NODE_R * 2.5))
    const key = `${laneIdx}:${bx}`
    if (!buckets.has(key)) buckets.set(key, [])
    buckets.get(key)!.push(node.id)
  }

  const positions = new Map<string, Pos>()
  for (const [key, ids] of buckets) {
    const laneIdx = parseInt(key.split(':')[0])
    const center = (laneIdx + 0.5) * laneH
    const lo = laneIdx * laneH + NODE_R + 2
    const hi = (laneIdx + 1) * laneH - NODE_R - 2
    for (let i = 0; i < ids.length; i++) {
      const offset = i === 0 ? 0 : Math.ceil(i / 2) * STAGGER * (i % 2 === 1 ? -1 : 1)
      positions.set(ids[i], {
        x: baseX.get(ids[i])!,
        y: Math.max(lo, Math.min(hi, center + offset)),
      })
    }
  }

  return positions
}

function parseRGBA(s: string): [number, number, number, number] {
  const m = s.match(/rgba?\(([^)]+)\)/)
  if (!m) return [0, 0, 0, 1]
  const parts = m[1].split(',').map(p => parseFloat(p.trim()))
  return [parts[0] || 0, parts[1] || 0, parts[2] || 0, parts[3] ?? 1]
}

function lerpRGBA(c1: string, c2: string, t: number): string {
  const [r1, g1, b1, a1] = parseRGBA(c1)
  const [r2, g2, b2, a2] = parseRGBA(c2)
  const r = r1 + (r2 - r1) * t
  const g = g1 + (g2 - g1) * t
  const b = b1 + (b2 - b1) * t
  const a = a1 + (a2 - a1) * t
  return `rgba(${r | 0},${g | 0},${b | 0},${a.toFixed(3)})`
}

function computeSessionBands(
  positions: Map<string, Pos>,
  nodes: Map<string, ArgumentNode>,
): Map<string, { x1: number; x2: number }> {
  const bands = new Map<string, { x1: number; x2: number }>()
  for (const [id, pos] of positions) {
    const node = nodes.get(id)
    if (!node) continue
    const sid = node.session_id
    const band = bands.get(sid)
    if (!band) {
      bands.set(sid, { x1: pos.x, x2: pos.x })
    } else {
      band.x1 = Math.min(band.x1, pos.x)
      band.x2 = Math.max(band.x2, pos.x)
    }
  }
  return bands
}

export function useTemporalGraph(
  nodes: Map<string, ArgumentNode>,
  edges: Map<string, ArgumentEdge>,
  traceNodeIds: Set<string>,
  selectedNodeId: string | null = null,
  communityMap?: Map<string, number>,
  communityColorMode?: boolean,
): UseTemporalGraphResult {
  const canvasRef = useRef<HTMLCanvasElement>(null!)
  const transformRef = useRef<d3zoom.ZoomTransform>(d3zoom.zoomIdentity)
  const [transform, setTransform] = useState<d3zoom.ZoomTransform>(d3zoom.zoomIdentity)
  const zoomRef = useRef<d3zoom.ZoomBehavior<HTMLCanvasElement, unknown> | null>(null)
  const targetPositionsRef = useRef<Map<string, Pos>>(new Map())
  const drawnPositionsRef = useRef<Map<string, Pos>>(new Map())
  const springsRef = useRef<Map<string, Spring>>(new Map())
  const dimRef = useRef<Map<string, number>>(new Map())
  const edgeDimRef = useRef<Map<string, number>>(new Map())
  const contentDimsRef = useRef({ w: 0, h: 0 })
  const viewportRef = useRef({ w: 0, h: 0 })
  const nodesRef = useRef(nodes)
  const edgesRef = useRef(edges)
  const traceRef = useRef(traceNodeIds)
  const selectedRef = useRef<string | null>(selectedNodeId)
  const communityMapRef = useRef(communityMap)
  const communityColorModeRef = useRef(communityColorMode)
  const rafRef = useRef<number>(0)
  const drawRef = useRef<() => void>(() => {})
  const lastFrameRef = useRef<number>(performance.now())
  const lastLayoutKeyRef = useRef<string>('')

  nodesRef.current = nodes
  edgesRef.current = edges
  traceRef.current = traceNodeIds
  selectedRef.current = selectedNodeId
  communityMapRef.current = communityMap
  communityColorModeRef.current = communityColorMode

  const draw = useCallback(() => {
    const canvas = canvasRef.current
    if (!canvas) return
    const ctx = canvas.getContext('2d')
    if (!ctx) return

    const dpr = window.devicePixelRatio || 1
    const W = canvas.clientWidth
    const H = canvas.clientHeight
    const contentW = W - MARGIN.left - MARGIN.right
    const contentH = H - MARGIN.top - MARGIN.bottom
    if (contentW <= 0 || contentH <= 0) return

    const curNodes = nodesRef.current
    const curEdges = edgesRef.current
    const curTrace = traceRef.current
    const curSelected = selectedRef.current
    const hasTrace = curTrace.size > 0 || curSelected !== null
    const t = transformRef.current

    const layoutKey = `${curNodes.size}:${contentW}:${contentH}`
    if (layoutKey !== lastLayoutKeyRef.current) {
      const newTargets = computePositions(curNodes, contentW, contentH)
      targetPositionsRef.current = newTargets
      const center: Pos = { x: contentW / 2, y: contentH / 2 }
      const springs = springsRef.current
      for (const [id, pos] of newTargets) {
        let s = springs.get(id)
        if (!s) {
          const existing = drawnPositionsRef.current.get(id)
          const start = existing ?? center
          s = { x: makeSpring(start.x), y: makeSpring(start.y) }
          springs.set(id, s)
        }
        s.x.set(pos.x)
        s.y.set(pos.y)
      }
      for (const id of [...springs.keys()]) {
        if (!newTargets.has(id)) springs.delete(id)
      }
      lastLayoutKeyRef.current = layoutKey
    }

    const now = performance.now()
    const dt = Math.min(0.064, Math.max(0.001, (now - lastFrameRef.current) / 1000))
    lastFrameRef.current = now

    const drawn = new Map<string, Pos>()
    for (const [id, s] of springsRef.current) {
      drawn.set(id, { x: s.x.step(dt), y: s.y.step(dt) })
    }
    drawnPositionsRef.current = drawn

    const dims = dimRef.current
    const edgeDims = edgeDimRef.current
    for (const id of curNodes.keys()) {
      const target = hasTrace && !curTrace.has(id) && id !== curSelected ? 1 : 0
      const cur = dims.get(id) ?? target
      dims.set(id, cur + (target - cur) * DIM_LERP)
    }
    for (const edge of curEdges.values()) {
      const target = hasTrace && !curTrace.has(edge.source_node_id) && !curTrace.has(edge.target_node_id) ? 1 : 0
      const cur = edgeDims.get(edge.id) ?? target
      edgeDims.set(edge.id, cur + (target - cur) * DIM_LERP)
    }

    const laneH = contentH / LANE_ORDER.length

    ctx.save()
    ctx.clearRect(0, 0, canvas.width, canvas.height)
    ctx.scale(dpr, dpr)

    ctx.beginPath()
    ctx.moveTo(MARGIN.left, MARGIN.top)
    ctx.lineTo(MARGIN.left, MARGIN.top + contentH)
    ctx.strokeStyle = 'rgba(255,255,255,0.12)'
    ctx.lineWidth = 1
    ctx.stroke()

    ctx.save()
    ctx.beginPath()
    ctx.rect(0, MARGIN.top, MARGIN.left, contentH)
    ctx.clip()
    ctx.translate(MARGIN.left + t.x, MARGIN.top + t.y)
    ctx.scale(t.k, t.k)
    const labelContentX = -t.x / t.k
    ctx.font = clampedFont(t.k, 11)
    ctx.fillStyle = 'rgba(200,200,220,0.85)'
    ctx.textAlign = 'right'
    ctx.textBaseline = 'middle'
    for (let i = 0; i < LANE_ORDER.length; i++) {
      ctx.fillText(LANE_ORDER[i], labelContentX - 8 / t.k, (i + 0.5) * laneH)
    }
    ctx.restore()

    ctx.save()
    ctx.beginPath()
    ctx.rect(MARGIN.left, MARGIN.top, contentW, contentH)
    ctx.clip()
    ctx.translate(MARGIN.left + t.x, MARGIN.top + t.y)
    ctx.scale(t.k, t.k)

    for (let i = 0; i < LANE_ORDER.length; i++) {
      const sy = i * laneH
      if (i % 2 === 0) {
        ctx.fillStyle = 'rgba(255,255,255,0.04)'
        ctx.fillRect(0, sy, contentW, laneH)
      }
      ctx.beginPath()
      ctx.moveTo(0, sy)
      ctx.lineTo(contentW, sy)
      ctx.strokeStyle = 'rgba(255,255,255,0.06)'
      ctx.lineWidth = clampedStroke(t.k, 1)
      ctx.stroke()
    }
    const bottomY = LANE_ORDER.length * laneH
    ctx.beginPath()
    ctx.moveTo(0, bottomY)
    ctx.lineTo(contentW, bottomY)
    ctx.strokeStyle = 'rgba(255,255,255,0.06)'
    ctx.lineWidth = clampedStroke(t.k, 1)
    ctx.stroke()

    const sessionBands = computeSessionBands(drawn, curNodes)
    const sortedBands = Array.from(sessionBands.entries()).sort((a, b) => a[1].x1 - b[1].x1)
    const SESSION_FILLS = ['rgba(99,102,241,0.10)', 'rgba(20,184,166,0.10)', 'rgba(245,158,11,0.10)', 'rgba(239,68,68,0.10)']
    for (let bi = 0; bi < sortedBands.length; bi++) {
      const [sid, band] = sortedBands[bi]
      const pad = NODE_R * 3
      const bx1 = band.x1 - pad
      const bx2 = band.x2 + pad
      ctx.fillStyle = SESSION_FILLS[bi % SESSION_FILLS.length] ?? 'rgba(128,128,128,0.10)'
      ctx.fillRect(bx1, 0, bx2 - bx1, contentH)
      if (bi > 0) {
        ctx.beginPath()
        ctx.moveTo(bx1, 0)
        ctx.lineTo(bx1, contentH)
        ctx.strokeStyle = 'rgba(255,255,255,0.10)'
        ctx.lineWidth = clampedStroke(t.k, 1)
        ctx.stroke()
      }
      ctx.font = clampedFont(t.k, 10)
      ctx.fillStyle = 'rgba(200,200,220,0.5)'
      ctx.textAlign = 'center'
      ctx.textBaseline = 'top'
      ctx.fillText(sid, (bx1 + bx2) / 2, 4 / t.k)
    }

    for (const edge of curEdges.values()) {
      const sp = drawn.get(edge.source_node_id)
      const tp = drawn.get(edge.target_node_id)
      if (!sp || !tp) continue

      const dim = edgeDims.get(edge.id) ?? 0
      const fullColor = EDGE_COLORS[edge.edge_type]

      const dx = tp.x - sp.x
      const arcH = Math.min(MAX_ARC_SCREEN_PX / t.k, Math.abs(dx) * 0.25)
      const cpx = (sp.x + tp.x) / 2
      const cpy = (sp.y + tp.y) / 2 - (dx >= 0 ? arcH : -arcH)

      ctx.beginPath()
      ctx.moveTo(sp.x, sp.y)
      ctx.quadraticCurveTo(cpx, cpy, tp.x, tp.y)
      ctx.strokeStyle = lerpRGBA(fullColor, DIM_EDGE_COLOR, dim)
      ctx.lineWidth = clampedStroke(t.k, 1.5)

      const pDash = provenanceDashes(edge.provenance)
      if (pDash.length > 0) {
        ctx.setLineDash(clampedDash(t.k, pDash))
      } else if (edge.edge_type === 'Refutes') {
        ctx.setLineDash(clampedDash(t.k, [6, 3]))
      } else {
        ctx.setLineDash([])
      }
      ctx.stroke()
      ctx.setLineDash([])

      if (edge.edge_type === 'Resolves' && dim < 0.5) {
        const atx = tp.x - cpx
        const aty = tp.y - cpy
        const alen = Math.hypot(atx, aty)
        if (alen > 0) {
          const ux = atx / alen
          const uy = aty / alen
          const r = 8 / t.k
          ctx.beginPath()
          ctx.moveTo(tp.x, tp.y)
          ctx.lineTo(tp.x - ux * r - uy * r * 0.5, tp.y - uy * r + ux * r * 0.5)
          ctx.lineTo(tp.x - ux * r + uy * r * 0.5, tp.y - uy * r - ux * r * 0.5)
          ctx.closePath()
          ctx.fillStyle = EDGE_COLORS.Resolves
          ctx.fill()
        }
      }
    }

    const cm = communityMapRef.current
    const ccm = communityColorModeRef.current
    const useCommunityColor = ccm && cm && cm.size > 0

    for (const [id, pos] of drawn) {
      const node = curNodes.get(id)
      if (!node) continue
      const dim = dims.get(id) ?? 0
      const color = useCommunityColor
        ? COMMUNITY_COLORS[(cm!.get(id) ?? 0) % COMMUNITY_COLORS.length]
        : NODE_COLORS[node.node_type]
      const glow = useCommunityColor
        ? hexAlpha(color, 0.6)
        : NODE_GLOW[node.node_type]

      ctx.beginPath()
      ctx.arc(pos.x, pos.y, NODE_R, 0, 2 * Math.PI)
      ctx.shadowBlur = (1 - dim) * 15
      ctx.shadowColor = glow
      ctx.fillStyle = lerpRGBA(color, DIM_NODE_COLOR, dim)
      ctx.fill()
      ctx.shadowBlur = 0

      if (dim < 0.5) {
        ctx.font = clampedFont(t.k, 10)
        ctx.fillStyle = `rgba(255,255,255,${(0.85 * (1 - dim)).toFixed(3)})`
        ctx.textAlign = 'center'
        ctx.textBaseline = 'top'
        ctx.fillText(
          node.label.length > 22 ? node.label.slice(0, 22) + '…' : node.label,
          pos.x,
          pos.y + NODE_R + 3 / t.k,
        )
      }
    }

    ctx.restore()

    if (curSelected) {
      const pos = drawn.get(curSelected)
      if (pos) {
        ctx.save()
        ctx.beginPath()
        ctx.rect(MARGIN.left, MARGIN.top, contentW, contentH)
        ctx.clip()
        ctx.translate(MARGIN.left + t.x, MARGIN.top + t.y)
        ctx.scale(t.k, t.k)
        ctx.beginPath()
        ctx.arc(pos.x, pos.y, NODE_R + 4 / t.k, 0, 2 * Math.PI)
        ctx.lineWidth = clampedStroke(t.k, 1.5)
        ctx.strokeStyle = 'rgba(255,255,255,0.9)'
        ctx.stroke()
        ctx.restore()
      }
    }

    ctx.restore()
  }, [])

  useEffect(() => {
    drawRef.current = draw
  }, [draw])

  useEffect(() => {
    const canvas = canvasRef.current
    if (!canvas) return

    const dpr = window.devicePixelRatio || 1
    if (canvas.clientWidth > 0 && canvas.clientHeight > 0) {
      canvas.width = canvas.clientWidth * dpr
      canvas.height = canvas.clientHeight * dpr
    }

    const zoom = d3zoom
      .zoom<HTMLCanvasElement, unknown>()
      .scaleExtent([0.1, 8])
      .on('zoom', (event: d3zoom.D3ZoomEvent<HTMLCanvasElement, unknown>) => {
        transformRef.current = event.transform
        setTransform(event.transform)
        drawRef.current()
      })

    zoomRef.current = zoom

    const setupTranslateExtent = (cw: number, ch: number) => {
      const cW = cw - MARGIN.left - MARGIN.right
      const cH = ch - MARGIN.top - MARGIN.bottom
      if (cW <= 0 || cH <= 0) return
      const PAD = 80
      zoom.translateExtent([
        [-PAD, -PAD],
        [cW + PAD, cH + PAD],
      ])
    }

    const initCW = canvas.clientWidth
    const initCH = canvas.clientHeight
    viewportRef.current = { w: initCW, h: initCH }
    setupTranslateExtent(initCW, initCH)
    contentDimsRef.current = {
      w: initCW - MARGIN.left - MARGIN.right,
      h: initCH - MARGIN.top - MARGIN.bottom,
    }

    d3selection.select(canvas).on('.zoom', null)
    d3selection.select(canvas).call(zoom)

    const ro = new ResizeObserver(() => {
      const cw = canvas.clientWidth
      const ch = canvas.clientHeight
      if (cw > 0 && ch > 0) {
        canvas.width = cw * dpr
        canvas.height = ch * dpr
        viewportRef.current = { w: cw, h: ch }
        contentDimsRef.current = {
          w: cw - MARGIN.left - MARGIN.right,
          h: ch - MARGIN.top - MARGIN.bottom,
        }
        setupTranslateExtent(cw, ch)
      }
      drawRef.current()
    })
    ro.observe(canvas)

    const loop = () => {
      drawRef.current()
      rafRef.current = requestAnimationFrame(loop)
    }
    rafRef.current = requestAnimationFrame(loop)

    return () => {
      ro.disconnect()
      cancelAnimationFrame(rafRef.current)
    }
  }, [])

  const getNodeAtPoint = useCallback((mx: number, my: number): ArgumentNode | null => {
    const t = transformRef.current
    const cx = (mx - MARGIN.left - t.x) / t.k
    const cy = (my - MARGIN.top - t.y) / t.k
    const hitRadius = HIT_RADIUS_SCREEN_PX / t.k
    let closest: ArgumentNode | null = null
    let closestDist = Infinity
    for (const [id, pos] of drawnPositionsRef.current) {
      const d = Math.hypot(pos.x - cx, pos.y - cy)
      if (d < hitRadius && d < closestDist) {
        closestDist = d
        closest = nodesRef.current.get(id) ?? null
      }
    }
    return closest
  }, [])

  const getSessionAtPoint = useCallback((mx: number, my: number): string | null => {
    const t = transformRef.current
    const cx = (mx - MARGIN.left - t.x) / t.k
    const cy = (my - MARGIN.top - t.y) / t.k
    const contentH = contentDimsRef.current.h
    if (cy < 0 || cy > contentH) return null
    const positions = drawnPositionsRef.current
    if (positions.size === 0) return null
    const bands = computeSessionBands(positions, nodesRef.current)
    for (const [sid, band] of bands) {
      const pad = NODE_R * 3
      if (cx >= band.x1 - pad && cx <= band.x2 + pad) return sid
    }
    return null
  }, [])

  const controls = useMemo<GraphControls>(() => ({
    zoomIn() {
      const z = zoomRef.current
      const canvas = canvasRef.current
      if (!z || !canvas) return
      d3selection.select(canvas).transition().duration(200).call(z.scaleBy, 1.4)
    },
    zoomOut() {
      const z = zoomRef.current
      const canvas = canvasRef.current
      if (!z || !canvas) return
      d3selection.select(canvas).transition().duration(200).call(z.scaleBy, 1 / 1.4)
    },
    fit() {
      const z = zoomRef.current
      const canvas = canvasRef.current
      if (!z || !canvas) return
      const positions = targetPositionsRef.current
      if (positions.size === 0) return
      let minX = Infinity, minY = Infinity, maxX = -Infinity, maxY = -Infinity
      for (const pos of positions.values()) {
        if (pos.x < minX) minX = pos.x
        if (pos.y < minY) minY = pos.y
        if (pos.x > maxX) maxX = pos.x
        if (pos.y > maxY) maxY = pos.y
      }
      const bbox: BBox = { x: minX, y: minY, w: maxX - minX, h: maxY - minY }
      const v = viewportRef.current
      const tr = fitTransform(bbox, v, 80)
      d3selection.select(canvas).transition().duration(300).call(z.transform, tr)
    },
    reset() {
      const z = zoomRef.current
      const canvas = canvasRef.current
      if (!z || !canvas) return
      d3selection.select(canvas).transition().duration(200).call(z.transform, d3zoom.zoomIdentity)
    },
    focusOnNode(id: string) {
      const z = zoomRef.current
      const canvas = canvasRef.current
      if (!z || !canvas) return
      const pos = targetPositionsRef.current.get(id)
      if (!pos) return
      const v = viewportRef.current
      const targetK = 1.6
      const tx = v.w / 2 - MARGIN.left - pos.x * targetK
      const ty = v.h / 2 - MARGIN.top - pos.y * targetK
      const tr = d3zoom.zoomIdentity.translate(tx, ty).scale(targetK)
      d3selection.select(canvas).transition().duration(400).call(z.transform, tr)
    },
    focusOnSession(sessionId: string) {
      const z = zoomRef.current
      const canvas = canvasRef.current
      if (!z || !canvas) return
      const positions = targetPositionsRef.current
      const curNodes = nodesRef.current
      let minX = Infinity, maxX = -Infinity
      for (const [id, pos] of positions) {
        const node = curNodes.get(id)
        if (node?.session_id === sessionId) {
          if (pos.x < minX) minX = pos.x
          if (pos.x > maxX) maxX = pos.x
        }
      }
      if (minX === Infinity) return
      const pad = NODE_R * 6
      const cH = contentDimsRef.current.h
      const bbox: BBox = { x: minX - pad, y: 0, w: maxX - minX + pad * 2, h: cH }
      const v = viewportRef.current
      const tr = fitTransform(bbox, v, 80)
      d3selection.select(canvas).transition().duration(400).call(z.transform, tr)
    },
  }), [])

  return {
    canvasRef,
    getNodeAtPoint,
    getSessionAtPoint,
    controls,
    transform,
  }
}
