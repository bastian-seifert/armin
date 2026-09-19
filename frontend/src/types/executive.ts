export type DecisionStatus = 'Pending' | 'Validated' | 'Blocked' | 'Superseded';

export interface Decision {
  id: string;
  label: string;
  rationale: string;
  status: DecisionStatus;
  owner: string | null;
  session_id: string;
  timestamp: number;
  blocked_work: string[];
  validation_criteria: string[];
}

export type ValidationStatus = 'Unvalidated' | 'Partial' | 'Validated';

export interface Risk {
  node_id: string;
  label: string;
  impact_score: number;
  validation_status: ValidationStatus;
  debt_type: 'UnresolvedOpenItem';
  session_id: string;
}

export interface DebtDelta {
  new: number;
  resolved: number;
  persisted: number;
}

export interface ExecutiveSummary {
  decisions: Decision[];
  risks: Risk[];
  debt_delta: DebtDelta;
  session_id: string;
  prior_session_id: string | null;
  generated_at: number;
}

export interface GraphDiff {
  added: { nodes: string[]; edges: string[] };
  removed: { nodes: string[]; edges: string[] };
  changed: { node_id: string; fields: string[] }[];
}
