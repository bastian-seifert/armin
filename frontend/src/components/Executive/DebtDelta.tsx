import type { DebtDelta } from '../../types/executive';

interface Props {
  delta: DebtDelta;
  priorSessionId: string | null;
}

const METRICS = [
  { key: 'new' as const, label: 'New', color: 'text-red-400', bg: 'bg-red-600/20', icon: '+' },
  { key: 'resolved' as const, label: 'Resolved', color: 'text-emerald-400', bg: 'bg-emerald-600/20', icon: '−' },
  { key: 'persisted' as const, label: 'Persisted', color: 'text-amber-400', bg: 'bg-amber-600/20', icon: '⟳' },
] as const;

export function DebtDeltaView({ delta, priorSessionId }: Props) {
  const sessionLabel = priorSessionId ?? 'previous';

  return (
    <div className="rounded-lg border border-gray-800 bg-gray-900/60 p-4">
      <div className="flex items-center justify-between mb-3">
        <span className="text-xs font-semibold text-gray-500 uppercase tracking-wider">
          Debt Delta vs {sessionLabel}
        </span>
        <span className="text-lg font-bold font-mono text-white">
          {delta.new - delta.resolved >= 0 ? '+' : ''}{delta.new - delta.resolved}
        </span>
      </div>
      <div className="flex gap-2">
        {METRICS.map(({ key, label, color, icon }) => (
          <div
            key={key}
            className="flex-1 rounded-lg border p-3 text-center transition-colors"
            style={{ borderColor: color.replace('text-', '').replace('-400', '-600') + '40' }}
          >
            <div className="flex items-center justify-center gap-1 mb-1">
              <span className="text-lg">{icon}</span>
              <span className={`text-xl font-bold font-mono ${color}`}>
                {delta[key]}
              </span>
            </div>
            <div className="text-[10px] font-medium text-gray-500 uppercase tracking-wider">{label}</div>
          </div>
        ))}
      </div>
      <div className="mt-3 h-1.5 bg-gray-800 rounded-full overflow-hidden">
        <div
          className="h-full bg-gradient-to-r from-red-500 via-amber-500 to-emerald-500"
          style={{
            width: `${delta.persisted > 0 ? 100 * (delta.new / (delta.new + delta.resolved + delta.persisted)) : 0}%`,
          }}
        />
      </div>
    </div>
  );
}
