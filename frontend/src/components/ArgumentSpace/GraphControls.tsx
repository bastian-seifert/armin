import { Maximize2, Minus, Plus, RotateCcw } from 'lucide-react'
import type { GraphControls } from '../../hooks/useTemporalGraph'

interface Props {
  controls: GraphControls
  k: number
}

export function GraphControls({ controls, k }: Props) {
  const btn = 'w-8 h-8 flex items-center justify-center text-gray-300 hover:bg-gray-800 hover:text-white transition-colors'
  return (
    <div className="absolute bottom-3 right-3 z-10 bg-gray-900/85 border border-gray-700 rounded-lg shadow-xl backdrop-blur-sm">
      <div className="flex">
        <button onClick={controls.zoomIn} title="Zoom in (+)" className={`${btn} border-r border-gray-700 rounded-tl-lg`}>
          <Plus size={14} />
        </button>
        <button onClick={controls.zoomOut} title="Zoom out (−)" className={`${btn} border-r border-gray-700`}>
          <Minus size={14} />
        </button>
        <button onClick={controls.fit} title="Fit to content (f)" className={`${btn} border-r border-gray-700`}>
          <Maximize2 size={14} />
        </button>
        <button onClick={controls.reset} title="Reset view (0)" className={`${btn} rounded-tr-lg`}>
          <RotateCcw size={14} />
        </button>
      </div>
      <div className="px-2 py-1 text-center text-[10px] font-mono text-gray-500 border-t border-gray-800">
        k = {k.toFixed(2)}×
      </div>
    </div>
  )
}
