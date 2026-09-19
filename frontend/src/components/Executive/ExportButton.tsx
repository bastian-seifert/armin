import { useState } from 'react';
import type { ExecutiveSummary } from '../../types/executive';

interface Props {
  summary: ExecutiveSummary;
}

function formatDecision(d: ExecutiveSummary['decisions'][0]): string {
  const lines = [
    `### ${d.label}`,
    `**Status:** ${d.status}`,
    `**Owner:** ${d.owner ?? 'Unassigned'}`,
    `**Session:** ${d.session_id}`,
    `**Rationale:** ${d.rationale}`,
  ];
  if (d.blocked_work.length > 0) {
    lines.push(`**Blocked Work:** ${d.blocked_work.join(', ')}`);
  }
  if (d.validation_criteria.length > 0) {
    lines.push(`**Validation Needed:** ${d.validation_criteria.join(', ')}`);
  }
  return lines.join('\n');
}

function formatRisk(r: ExecutiveSummary['risks'][0]): string {
  return `- **${r.label}** (Impact: ${Math.round(r.impact_score)}/100) — ${r.debt_type}, ${r.validation_status}`;
}

function generateMarkdown(summary: ExecutiveSummary): string {
  const date = new Date(summary.generated_at).toLocaleString();

  const sections = [
    `# Executive Summary — ${summary.session_id}`,
    `*Generated: ${date} | Compared to: ${summary.prior_session_id ?? 'N/A'}*`,
    '',
    `## Debt Delta`,
    `- **New:** ${summary.debt_delta.new}`,
    `- **Resolved:** ${summary.debt_delta.resolved}`,
    `- **Persisted:** ${summary.debt_delta.persisted}`,
    `- **Net Change:** ${summary.debt_delta.new - summary.debt_delta.resolved >= 0 ? '+' : ''}${summary.debt_delta.new - summary.debt_delta.resolved}`,
    '',
    `## Decisions (${summary.decisions.length})`,
    ...summary.decisions.map(formatDecision),
    '',
    `## Top Risks (${summary.risks.length})`,
    ...summary.risks.map(formatRisk),
  ];

  return sections.join('\n');
}

export function ExportButton({ summary }: Props) {
  const [copied, setCopied] = useState(false);

  const handleCopy = async () => {
    const markdown = generateMarkdown(summary);
    await navigator.clipboard.writeText(markdown);
    setCopied(true);
    setTimeout(() => setCopied(false), 2000);
  };

  return (
    <button
      onClick={handleCopy}
      className="flex items-center gap-2 px-3 py-1.5 rounded-lg border border-gray-700 text-sm text-gray-300 hover:bg-gray-800 hover:border-gray-600 hover:text-white transition-colors"
      title="Copy executive summary as Markdown"
    >
      <svg className="w-4 h-4" fill="none" stroke="currentColor" viewBox="0 0 24 24">
        <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M8 5H6a2 2 0 00-2 2v12a2 2 0 002 2h10a2 2 0 002-2v-1M8 5a2 2 0 002 2h2a2 2 0 002-2M8 5a2 2 0 012-2h2a2 2 0 012 2m0 0h2a2 2 0 012 2v3m2 4H10m0 0l3-3m-3 3l3 3" />
      </svg>
      {copied ? 'Copied!' : 'Export MD'}
    </button>
  );
}
