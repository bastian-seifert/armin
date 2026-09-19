import { useEffect, useState, useRef } from 'react';
import type { ExecutiveSummary } from '../types/executive';

const API_BASE = (import.meta.env.VITE_API_URL ?? 'http://localhost:8080/query')
  .replace('/query', '');

export function useExecutiveData(): {
  summary: ExecutiveSummary | null;
  loading: boolean;
  error: string | null;
  refresh: () => void;
} {
  const [summary, setSummary] = useState<ExecutiveSummary | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const mountedRef = useRef(true);

  const fetchSummary = async () => {
    setLoading(true);
    setError(null);

    try {
      const resp = await fetch(`${API_BASE}/summary`);
      if (resp.ok) {
        const data = (await resp.json()) as ExecutiveSummary;
        // Convert generated_at from Unix seconds to JS ms
        const ms = data.generated_at * 1000;
        if (mountedRef.current) {
          setSummary({ ...data, generated_at: ms });
          setLoading(false);
        }
        return;
      }
    } catch {
      // Fall through to individual endpoints
    }

    try {
      const [decisionsResp, risksResp] = await Promise.all([
        fetch(`${API_BASE}/decisions`),
        fetch(`${API_BASE}/risks`),
      ]);

      if (decisionsResp.ok && risksResp.ok) {
        const decisions = await decisionsResp.json();
        const risks = await risksResp.json();
        if (mountedRef.current) {
          setSummary({
            decisions,
            risks,
            debt_delta: { new: 0, resolved: 0, persisted: 0 },
            session_id: 'Unknown',
            prior_session_id: null,
            generated_at: Date.now(),
          });
          setLoading(false);
        }
        return;
      }
    } catch {
      // Fall through to error
    }

    if (mountedRef.current) {
      setError('Could not reach backend — is the server running?');
      setLoading(false);
    }
  };

  useEffect(() => {
    mountedRef.current = true;
    fetchSummary();
    const interval = setInterval(fetchSummary, 30000);
    return () => {
      mountedRef.current = false;
      clearInterval(interval);
    };
  }, []);

  return { summary, loading, error, refresh: fetchSummary };
}
