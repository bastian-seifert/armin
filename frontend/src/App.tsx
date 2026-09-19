import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import { Routes, Route, Navigate } from 'react-router-dom'
import { GraphProvider, useGraph } from './context/GraphContext'
import { Feed } from './components/Feed/Feed'
import { ArgumentSpace, type ArgumentSpaceHandle } from './components/ArgumentSpace/ArgumentSpace'
import { DebtHUD } from './components/DebtHUD/DebtHUD'
import { NodeMenu } from './components/NodeMenu/NodeMenu'
import { ExecutiveSummary } from './components/Executive/ExecutiveSummary'
import { LayoutSelector, type LayoutMode } from './components/ui/LayoutSelector'
import { CommandPalette } from './components/ui/CommandPalette'
import { ResizablePanel } from './components/ui/ResizablePanel'
import { Toaster } from 'sonner'

function readLayoutMode(): LayoutMode {
  const hash = window.location.hash.replace('#', '')
  if (hash === 'live' || hash === 'review' || hash === 'executive') return hash
  return 'live'
}

function StreamView() {
  const { state } = useGraph()
  const { edges, connected } = state

  const [layoutMode, setLayoutMode] = useState<LayoutMode>(readLayoutMode)
  const [menuSelectedNodeId, setMenuSelectedNodeId] = useState<string | null>(null)
  const [paletteOpen, setPaletteOpen] = useState(false)
  const argSpaceRef = useRef<ArgumentSpaceHandle>(null)

  const handleLayoutChange = useCallback((mode: LayoutMode) => {
    setLayoutMode(mode)
    window.location.hash = mode
  }, [])

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const target = e.target as HTMLElement | null
      if (target) {
        const tag = target.tagName
        if (tag === 'INPUT' || tag === 'TEXTAREA' || target.isContentEditable) return
      }
      if ((e.metaKey || e.ctrlKey) && e.key === 'k') {
        e.preventDefault()
        setPaletteOpen(p => !p)
        return
      }
      if (e.key === '1') handleLayoutChange('live')
      else if (e.key === '2') handleLayoutChange('review')
      else if (e.key === '3') handleLayoutChange('executive')
    }
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [handleLayoutChange])

  useEffect(() => {
    if (menuSelectedNodeId) argSpaceRef.current?.focusOnNode(menuSelectedNodeId)
  }, [menuSelectedNodeId])

  const menuHighlightedIds = useMemo(() => {
    if (!menuSelectedNodeId) return new Set<string>()
    const ids = new Set<string>([menuSelectedNodeId])
    for (const edge of edges.values()) {
      if (edge.source_node_id === menuSelectedNodeId) ids.add(edge.target_node_id)
      if (edge.target_node_id === menuSelectedNodeId) ids.add(edge.source_node_id)
    }
    return ids
  }, [menuSelectedNodeId, edges])

  const combinedHighlightedIds = useMemo(() => {
    if (menuHighlightedIds.size === 0 && state.traceNodeIds.size === 0) return new Set<string>()
    return new Set([...state.traceNodeIds, ...menuHighlightedIds])
  }, [state.traceNodeIds, menuHighlightedIds])

  const isExecutive = layoutMode === 'executive'

  const panelRatio = isExecutive ? 0.42 : layoutMode === 'review' ? 0.22 : 0.3

  return (
    <>
      <header className="flex items-center justify-between px-4 py-2 border-b border-gray-800 shrink-0">
        <div className="flex items-baseline gap-3">
          <span className="font-bold tracking-widest text-indigo-400 uppercase text-sm">ARMIN</span>
          <span className="text-xs text-gray-600">Agent Reasoning Memory &amp; Introspection Network</span>
        </div>
        <div className="flex items-center gap-3">
          <LayoutSelector mode={layoutMode} onChange={handleLayoutChange} />
          <div className="flex items-center gap-2 text-xs">
            <span
              className={`w-1.5 h-1.5 rounded-full ${connected ? 'bg-green-400' : 'bg-red-500'}`}
            />
            <span className="text-gray-500">{connected ? 'live' : 'disconnected'}</span>
          </div>
        </div>
      </header>

      <div className="flex flex-1 overflow-hidden">
        <ResizablePanel
          key={layoutMode}
          defaultRatio={panelRatio}
          left={
            isExecutive ? (
              <ExecutiveSummary
                onFocusNode={(id) => {
                  setMenuSelectedNodeId(id)
                  argSpaceRef.current?.focusOnNode(id)
                }}
              />
            ) : (
              <>
                <div className="px-3 py-2 border-b border-gray-800 shrink-0">
                  <h2 className="text-xs font-semibold text-gray-500 uppercase tracking-wider">The Feed</h2>
                </div>
                <div className="flex-1 overflow-hidden">
                  <Feed />
                </div>
              </>
            )
          }
          right={
            <>
              <div className="px-3 py-2 border-b border-gray-800 shrink-0">
                <h2 className="text-xs font-semibold text-gray-500 uppercase tracking-wider">
                  {isExecutive ? 'Graph Reference' : 'The Argument Space'}
                </h2>
              </div>
              <div className="flex-1 relative">
                <NodeMenu
                  selectedNodeId={menuSelectedNodeId}
                  onSelectNode={setMenuSelectedNodeId}
                />
                <ArgumentSpace ref={argSpaceRef} traceNodeIds={combinedHighlightedIds} />
              </div>
            </>
          }
        />
      </div>

      <div className={`shrink-0 ${isExecutive ? 'hidden' : ''}`}>
        <DebtHUD />
      </div>

      <CommandPalette
        open={paletteOpen}
        onClose={() => setPaletteOpen(false)}
        onChangeLayout={handleLayoutChange}
      />
    </>
  )
}

function HistoryView() {
  return (
    <div className="h-screen flex items-center justify-center bg-gray-950 text-gray-500">
      <p className="text-sm">History view — coming soon</p>
    </div>
  )
}

function AppContent() {
  return (
    <div className="h-screen flex flex-col bg-gray-950 text-gray-100">
      <Routes>
        <Route path="/" element={<Navigate to="/stream" replace />} />
        <Route path="/stream" element={<StreamView />} />
        <Route path="/history" element={<HistoryView />} />
        <Route path="*" element={<Navigate to="/stream" replace />} />
      </Routes>
      <Toaster position="bottom-right" />
    </div>
  )
}

export function App() {
  return (
    <GraphProvider>
      <AppContent />
    </GraphProvider>
  )
}
