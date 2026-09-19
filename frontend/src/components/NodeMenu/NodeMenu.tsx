import { useMemo, useState } from 'react'
import type { ArgumentNode, NodeType } from '../../types'
import { useGraph } from '../../context/GraphContext'
import { NODE_COLORS } from '../../lib/colors'

const CATEGORY_ORDER: NodeType[] = ['Decision', 'Rule', 'OpenItem']

interface Props {
  selectedNodeId: string | null
  onSelectNode: (id: string | null) => void
}

export function NodeMenu({ selectedNodeId, onSelectNode }: Props) {
  const { state } = useGraph()
  const nodes = state.nodes
  const [open, setOpen] = useState(false)

  const grouped = useMemo(() => {
    const map: Record<NodeType, ArgumentNode[]> = {
      Decision: [],
      Rule: [],
      OpenItem: [],
    }
    for (const node of nodes.values()) {
      map[node.node_type].push(node)
    }
    return map
  }, [nodes])

  return (
    <>
      {!open && (
        <button
          onClick={() => setOpen(true)}
          className="absolute top-3 left-3 z-20 bg-gray-900/90 border border-gray-700 rounded-lg px-2.5 py-1 text-xs text-gray-300 hover:text-white hover:bg-gray-800 transition-colors"
        >
          Nodes
        </button>
      )}

      {open && (
        <div className="absolute left-0 top-0 bottom-0 z-20 w-64 bg-gray-900/95 border-r border-gray-700 flex flex-col overflow-hidden">
          <div className="flex items-center justify-between px-3 py-2 border-b border-gray-800 shrink-0">
            <span className="text-xs font-semibold text-gray-500 uppercase tracking-wider">
              Nodes ({nodes.size})
            </span>
            <button
              onClick={() => { setOpen(false); onSelectNode(null) }}
              className="text-gray-500 hover:text-white text-sm leading-none px-1"
            >
              ✕
            </button>
          </div>

          <div className="flex-1 overflow-y-auto">
            {CATEGORY_ORDER.map(type => {
              const items = grouped[type]
              if (items.length === 0) return null
              const color = NODE_COLORS[type]

              return (
                <div key={type}>
                  <div className="sticky top-0 bg-gray-900/95 px-3 py-1.5 text-[10px] font-semibold uppercase tracking-wider text-gray-500 border-b border-gray-800/50 flex items-center gap-1.5">
                    <span className="w-2 h-2 rounded-full" style={{ backgroundColor: color }} />
                    {type}
                    <span className="text-gray-600 font-normal ml-auto">{items.length}</span>
                  </div>
                  {items.map(node => {
                    const isSelected = selectedNodeId === node.id
                    return (
                      <button
                        key={node.id}
                        onClick={() => onSelectNode(isSelected ? null : node.id)}
                        className={`w-full text-left px-3 py-1.5 text-xs border-b border-gray-800/30 flex items-center gap-2 transition-colors ${
                          isSelected
                            ? 'bg-gray-700/60 text-white'
                            : 'text-gray-400 hover:bg-gray-800/50 hover:text-gray-200'
                        }`}
                      >
                        <span
                          className="w-2 h-2 rounded-full shrink-0"
                          style={{ backgroundColor: color }}
                        />
                        <span className="truncate">{node.label}</span>
                      </button>
                    )
                  })}
                </div>
              )
            })}
          </div>
        </div>
      )}
    </>
  )
}
