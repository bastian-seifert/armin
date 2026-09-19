import { motion } from 'motion/react';
import type { Decision, DecisionStatus } from '../../types/executive';
import { formatRelativeTime } from '../../lib/time';

const STATUS_CONFIG: Record<DecisionStatus, { color: string; bg: string; icon: string }> = {
  Validated: { color: 'text-emerald-400', bg: 'bg-emerald-600/20', icon: '✓' },
  Pending: { color: 'text-amber-400', bg: 'bg-amber-600/20', icon: '○' },
  Blocked: { color: 'text-red-400', bg: 'bg-red-600/20', icon: '⛔' },
  Superseded: { color: 'text-gray-400', bg: 'bg-gray-600/20', icon: '⇄' },
};

interface Props {
  decisions: Decision[];
  onSelect?: (decision: Decision) => void;
}

export function DecisionTimeline({ decisions, onSelect }: Props) {
  if (decisions.length === 0) {
    return (
      <div className="text-center py-8 text-gray-500 text-sm">
        No decisions extracted yet
      </div>
    );
  }

  return (
    <div className="flex flex-col gap-3 max-h-[400px] overflow-y-auto pr-1">
      {decisions.map((decision, i) => {
        const cfg = STATUS_CONFIG[decision.status];
        const meetingLabel = decision.session_id;
        return (
          <motion.div
            key={decision.id}
            initial={{ opacity: 0, y: 10 }}
            animate={{ opacity: 1, y: 0 }}
            transition={{ delay: i * 0.05, duration: 0.2 }}
            onClick={() => onSelect?.(decision)}
            className={`group cursor-pointer rounded-lg border border-gray-800 bg-gray-900/60 p-3 transition-all hover:border-gray-700 hover:bg-gray-800/60 ${
              onSelect ? '' : 'cursor-default'
            }`}
          >
            <div className="flex items-start gap-3">
              <div className={`flex-shrink-0 w-8 h-8 rounded-full flex items-center justify-center text-xs font-bold ${cfg.bg} ${cfg.color}`}>
                {cfg.icon}
              </div>
              <div className="flex-1 min-w-0">
                <div className="flex items-center gap-2">
                  <h4 className="text-sm font-semibold text-white truncate">{decision.label}</h4>
                  <span className={`text-[10px] font-medium px-1.5 py-0.5 rounded ${cfg.bg} ${cfg.color}`}>
                    {decision.status}
                  </span>
                </div>
                <p className="mt-1 text-[11px] text-gray-400 line-clamp-2">{decision.rationale}</p>
                <div className="mt-2 flex flex-wrap items-center gap-2 text-[10px] text-gray-500">
                  <span className="flex items-center gap-1">
                    <span className="font-mono text-gray-400">{meetingLabel}</span>
                    <span>·</span>
                    <span>{formatRelativeTime(decision.timestamp)}</span>
                  </span>
                  {decision.owner && (
                    <span className="flex items-center gap-1">
                      <span>Owner:</span>
                      <span className="text-gray-300">{decision.owner}</span>
                    </span>
                  )}
                </div>
                {(decision.blocked_work.length > 0 || decision.validation_criteria.length > 0) && (
                  <div className="mt-2 pt-2 border-t border-gray-800 flex flex-wrap gap-2 text-[10px]">
                    {decision.blocked_work.slice(0, 2).map((work, idx) => (
                      <span key={idx} className="px-1.5 py-0.5 rounded bg-gray-800 text-gray-300 flex items-center gap-1">
                        <span className="text-red-400">⛭</span>
                        {work}
                      </span>
                    ))}
                    {decision.blocked_work.length > 2 && (
                      <span className="px-1.5 py-0.5 rounded bg-gray-800 text-gray-500">
                        +{decision.blocked_work.length - 2} more
                      </span>
                    )}
                    {decision.validation_criteria.slice(0, 1).map((criteria, idx) => (
                      <span key={idx} className="px-1.5 py-0.5 rounded bg-gray-800 text-gray-300 flex items-center gap-1">
                        <span className="text-amber-400">⏳</span>
                        {criteria}
                      </span>
                    ))}
                  </div>
                )}
              </div>
            </div>
          </motion.div>
        );
      })}
    </div>
  );
}