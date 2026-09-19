export type DebtTrend = 'Improving' | 'Worsening' | 'Stable';

export interface TeamDebtSummary {
  team_id: string;
  team_name: string;
  meeting_count: number;
  node_count: number;
  total_debt_score: number;
  debt_breakdown: Record<string, number>;
  top_risks: Risk[];
  trend: DebtTrend;
}

export interface CrossTeamContradiction {
  team_a: string;
  team_b: string;
  contradiction_count: number;
  top_claims: [string, string][];
  severity: number;
}

export interface SourceBreakdown {
  source_type: string;
  node_count: number;
}

export interface DebtDelta {
  new: number;
  resolved: number;
  persisted: number;
}

export interface Risk {
  node_id: string;
  label: string;
  impact_score: number;
  validation_status: string;
  debt_type: string;
  meeting_id: string;
}

export interface OrgHealthReport {
  total_debt_score: number;
  total_decisions: number;
  total_risks: number;
  total_cross_source_edges: number;
  team_summaries: TeamDebtSummary[];
  cross_team_contradictions: CrossTeamContradiction[];
  source_breakdown: SourceBreakdown[];
  trend_vs_previous: DebtDelta;
  generated_at: number;
}
