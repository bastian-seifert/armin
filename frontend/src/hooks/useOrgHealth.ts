import { useEffect, useState, useRef } from 'react';
import type { OrgHealthReport } from '../types/org';

const API_BASE = (import.meta.env.VITE_API_URL ?? 'http://localhost:8080/query')
  .replace('/query', '');

export function useOrgHealth(): {
  report: OrgHealthReport | null;
  loading: boolean;
  error: string | null;
  refresh: () => void;
} {
  const [report, setReport] = useState<OrgHealthReport | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const mountedRef = useRef(true);

  const fetchReport = async () => {
    setLoading(true);
    setError(null);

    try {
      const resp = await fetch(`${API_BASE}/org-health`);
      if (resp.ok) {
        const data = (await resp.json()) as OrgHealthReport;
        if (mountedRef.current) {
          setReport({ ...data, generated_at: data.generated_at * 1000 });
          setLoading(false);
        }
        return;
      }
    } catch {
      // fall through
    }

    if (mountedRef.current) {
      setError('Could not reach backend — is the server running?');
      setLoading(false);
    }
  };

  useEffect(() => {
    mountedRef.current = true;
    fetchReport();
    const interval = setInterval(fetchReport, 30000);
    return () => {
      mountedRef.current = false;
      clearInterval(interval);
    };
  }, []);

  return { report, loading, error, refresh: fetchReport };
}
