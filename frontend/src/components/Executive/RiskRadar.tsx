import { motion } from 'motion/react';
import type { Risk } from '../../types/executive';

const DEBT_TYPE_CONFIG: Record<Risk['debt_type'], { label: string; color: string; bg: string }> = {
  UnresolvedOpenItem: { color: 'text-amber-400', bg: 'bg-amber-600/20', label: 'Open Item' },
};

const VALIDATION_CONFIG: Record<Risk['validation_status'], { color: string; label: string }> = {
  Unvalidated: { color: 'text-gray-500', label: 'Unvalidated' },
  Partial: { color: 'text-amber-400', label: 'Partial' },
  Validated: { color: 'text-emerald-400', label: 'Validated' },
};

interface Props {
  risks: Risk[];
  onSelect?: (risk: Risk) => void;
}

export function RiskRadar({ risks, onSelect }: Props) {
  if (risks.length === 0) {
    return (
      <div className="text-center py-8 text-gray-500 text-sm">
        No risks detected
      </div>
    );
  }

  const sortedRisks = [...risks].sort((a, b) => b.impact_score - a.impact_score);

  return (
    <div className="flex flex-col gap-3 max-h-[400px] overflow-y-auto pr-1">
      {sortedRisks.map((risk, i) => {
        const debtCfg = DEBT_TYPE_CONFIG[risk.debt_type];
        const valCfg = VALIDATION_CONFIG[risk.validation_status];
        const impactPct = Math.min(100, risk.impact_score);
        return (
          <motion.div
            key={risk.node_id}
            initial={{ opacity: 0, x: -10 }}
            animate={{ opacity: 1, x: 0 }}
            transition={{ delay: i * 0.05, duration: 0.2 }}
            onClick={() => onSelect?.(risk)}
            className={`group cursor-pointer rounded-lg border border-gray-800 bg-gray-900/60 p-3 transition-all hover:border-gray-700 hover:bg-gray-800/60 ${
              onSelect ? '' : 'cursor-default'
            }`}
          >
            <div className="flex items-start gap-3">
              <div className="flex-shrink-0 w-12 text-right">
                <div className="text-lg font-bold font-mono text-white">{impactPct}</div>
                <div className="text-[10px] text-gray-500">impact</div>
              </div>
              <div className="flex-1 min-w-0">
                <div className="flex items-center gap-2">
                  <h4 className="text-sm font-semibold text-white truncate">{risk.label}</h4>
                  <span className={`text-[10px] font-medium px-1.5 py-0.5 rounded ${debtCfg.bg} ${debtCfg.color}`}>
                    {debtCfg.label}
                  </span>
                </div>
                <div className="mt-2 h-1.5 bg-gray-800 rounded-full overflow-hidden">
                  <motion.div
                    initial={{ width: 0 }}
                    animate={{ width: `${impactPct}%` }}
                    transition={{ delay: 0.2 + i * 0.05, duration: 0.5, ease: 'easeOut' }}
                    className="h-full bg-gradient-to-r from-red-500 via-amber-500 to-emerald-500"
                  />
                </div>
                <div className="mt-2 flex items-center gap-3 text-[10px]">
                  <span className={`flex items-center gap-1 ${valCfg.color}`}>
                    <span className="w-1.5 h-1.5 rounded-full bg-current" />
                    {valCfg.label}
                  </span>
                </div>
              </div>
            </div>
          </motion.div>
        );
      })}
    </div>
  );
}