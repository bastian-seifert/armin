import { useState } from 'react'
import type { DebtItem } from '../../types'
import { useGraph } from '../../context/GraphContext'
import { DebtMetrics } from './DebtMetrics'
import { QueryPanel } from './QueryPanel'

export function DebtHUD() {
  const { state, dispatch } = useGraph()
  const { debtReport } = state
  const [selectedDebtItems, setSelectedDebtItems] = useState<DebtItem[] | null>(null)

  const handleDebtSelect = (items: DebtItem[]) => {
    if (items.length === 0) {
      setSelectedDebtItems(null)
      dispatch({ type: 'SET_TRACE', payload: new Set() })
    } else {
      setSelectedDebtItems(items)
      dispatch({ type: 'SET_TRACE', payload: new Set(items.flatMap(i => i.node_ids)) })
    }
  }

  const handleTraceResult = (nodeIds: Set<string>) => {
    dispatch({ type: 'SET_TRACE', payload: nodeIds })
  }

  return (
    <div className="flex items-start gap-6 px-4 py-3 bg-gray-900/90 border-t border-gray-800 min-h-[56px]">
      <DebtMetrics
        report={debtReport}
        onDebtSelect={handleDebtSelect}
        selectedItems={selectedDebtItems}
      />
      <div className="w-px self-stretch bg-gray-800" />
      <QueryPanel onTraceResult={handleTraceResult} />
    </div>
  )
}
