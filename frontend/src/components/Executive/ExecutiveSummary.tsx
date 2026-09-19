import { useEffect, useRef, useState } from 'react';
import { RefreshCcw } from 'lucide-react';
import { useExecutiveData } from '../../hooks/useExecutiveData';
import { useGraph } from '../../context/GraphContext';
import { DecisionTimeline } from './DecisionTimeline';
import { RiskRadar } from './RiskRadar';
import { DebtDeltaView } from './DebtDelta';
import { ExportButton } from './ExportButton';
import type { Decision, Risk } from '../../types/executive';

type Tab = 'decisions' | 'risks' | 'delta';

interface Props {
  onFocusNode?: (nodeId: string) => void;
}

export function ExecutiveSummary({ onFocusNode }: Props) {
  const { summary, loading, error, refresh } = useExecutiveData();
  const { state } = useGraph();
  const [tab, setTab] = useState<Tab>('decisions');
  const prevDebtRef = useRef(state.debtReport?.total_score);

  useEffect(() => {
    const currentScore = state.debtReport?.total_score;
    if (prevDebtRef.current !== undefined && currentScore !== prevDebtRef.current) {
      refresh();
    }
    prevDebtRef.current = currentScore;
  }, [state.debtReport, refresh]);

  if (loading && !summary) {
    return (
      <div className="flex-1 flex items-center justify-center">
        <div className="flex flex-col items-center gap-3 text-gray-500">
          <RefreshCcw className="w-6 h-6 animate-spin" />
          <span className="text-sm">Loading executive summary…</span>
        </div>
      </div>
    );
  }

  if (error && !summary) {
    return (
      <div className="flex-1 flex items-center justify-center">
        <div className="text-center text-gray-500 text-sm">
          <p className="mb-2">Failed to load executive summary</p>
          <button onClick={refresh} className="text-indigo-400 hover:text-indigo-300 underline">
            Retry
          </button>
        </div>
      </div>
    );
  }

  if (!summary) {
    return (
      <div className="flex-1 flex items-center justify-center">
        <div className="text-center text-gray-500 text-sm">
          <p>No data yet — start a stream to generate insights</p>
        </div>
      </div>
    );
  }

  const onSelectDecision = (decision: Decision) => {
    if (onFocusNode) onFocusNode(decision.id);
  };
  const onSelectRisk = (risk: Risk) => {
    if (onFocusNode) onFocusNode(risk.node_id);
  };

  const tabs: { key: Tab; label: string; count: number; color: string }[] = [
    { key: 'decisions', label: 'Decisions', count: summary.decisions.length, color: 'text-indigo-400' },
    { key: 'risks', label: 'Risks', count: summary.risks.length, color: 'text-red-400' },
    { key: 'delta', label: 'Debt Delta', count: 0, color: 'text-amber-400' },
  ];

  return (
    <div className="flex flex-col h-full">
      <div className="px-4 py-3 border-b border-gray-800 shrink-0">
        <div className="flex items-center justify-between mb-3">
          <div>
            <h2 className="text-sm font-bold text-white tracking-wide">Executive Summary</h2>
            <p className="text-[10px] text-gray-500 mt-0.5">
              {summary.session_id} vs {summary.prior_session_id ?? 'N/A'}
            </p>
          </div>
          <div className="flex items-center gap-2">
            <ExportButton summary={summary} />
            <button
              onClick={refresh}
              className="p-1.5 rounded-lg text-gray-500 hover:text-white hover:bg-gray-800 transition-colors"
              title="Refresh"
            >
              <RefreshCcw className="w-4 h-4" />
            </button>
          </div>
        </div>

        <div className="flex gap-1">
          {tabs.map(t => (
            <button
              key={t.key}
              onClick={() => setTab(t.key)}
              className={`px-3 py-1.5 rounded-lg text-xs font-medium transition-colors ${
                tab === t.key
                  ? 'bg-gray-800 text-gray-200'
                  : 'text-gray-500 hover:text-gray-300'
              }`}
            >
              <span className={`${tab === t.key ? t.color : ''}`}>{t.label}</span>
              {t.key !== 'delta' && (
                <span className={`ml-1.5 text-[10px] ${tab === t.key ? t.color : 'text-gray-600'}`}>
                  {t.count}
                </span>
              )}
            </button>
          ))}
        </div>
      </div>

      <div className="flex-1 overflow-y-auto px-4 py-3">
        {tab === 'decisions' && (
          <DecisionTimeline decisions={summary.decisions} onSelect={onSelectDecision} />
        )}
        {tab === 'risks' && (
          <RiskRadar risks={summary.risks} onSelect={onSelectRisk} />
        )}
        {tab === 'delta' && (
          <DebtDeltaView delta={summary.debt_delta} priorSessionId={summary.prior_session_id} />
        )}
      </div>
    </div>
  );
}
