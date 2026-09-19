import type { DebtItem, DebtReport } from '../../types'

interface Props {
  report: DebtReport | null
  onDebtSelect: (items: DebtItem[]) => void
  selectedItems: DebtItem[] | null
}

const LABELS: Record<string, string> = {
  UnresolvedOpenItem: 'open items',
}

const SEVERITY_COLORS: Record<string, string> = {
  High: 'text-red-400',
  Medium: 'text-amber-400',
  Low: 'text-gray-400',
}

export function DebtMetrics({ report, onDebtSelect, selectedItems }: Props) {
  if (!report) {
    return (
      <div className="text-xs text-gray-600">Reasoning debt: awaiting data…</div>
    )
  }

  const byType: Record<string, DebtItem[]> = {}
  report.items.forEach(item => {
    byType[item.debt_type] = [...(byType[item.debt_type] ?? []), item]
  })

  const scoreColor =
    report.total_score === 0
      ? 'text-green-400'
      : report.total_score < 5
      ? 'text-amber-400'
      : 'text-red-400'

  return (
    <div className="flex flex-col gap-1.5 text-xs min-w-0">
      <div className="flex items-center gap-4">
        <div className="flex items-baseline gap-1.5">
          <span className="text-gray-500 uppercase tracking-wider font-semibold text-[10px]">Debt</span>
          <span className={`text-lg font-bold font-mono ${scoreColor}`}>{report.total_score}</span>
        </div>
        <div className="flex flex-wrap gap-2">
          {Object.keys(byType).length === 0 ? (
            <span className="text-gray-500">no issues detected</span>
          ) : (
            Object.entries(byType).map(([type, items]) => (
              <button
                key={type}
                onClick={() => onDebtSelect(selectedItems?.[0]?.debt_type === type ? [] : items)}
                className={`px-2 py-0.5 rounded-full border text-[11px] transition-colors ${
                  selectedItems?.[0]?.debt_type === type
                    ? 'border-indigo-500 bg-indigo-900/40 text-indigo-300'
                    : 'border-gray-700 text-gray-400 hover:border-gray-500 hover:text-gray-200'
                }`}
              >
                {items.length} {LABELS[type] ?? type}
              </button>
            ))
          )}
        </div>
      </div>

      {selectedItems && selectedItems.length > 0 && (
        <div className="flex flex-col gap-1 max-h-32 overflow-y-auto pr-1">
          {selectedItems.map((item, i) => (
            <div key={i} className="flex items-start gap-2 text-[11px]">
              <span className={`shrink-0 font-semibold ${SEVERITY_COLORS[item.severity] ?? 'text-gray-400'}`}>
                [{item.severity}]
              </span>
              <span className="text-gray-300 leading-relaxed">{item.description}</span>
            </div>
          ))}
        </div>
      )}
    </div>
  )
}
