import { useEffect, useRef, useState } from 'react';
import { listen } from '@tauri-apps/api/event';
import type { DashboardRequest, DashboardSnapshot, MonitorApi, RankRow, TargetRequest } from './api';

interface QueryState<T> { key: string; data: T | null; loading: boolean; error: string | null }
function errorText(error: unknown): string { return error instanceof Error ? error.message : String(error); }

export function useDashboard(request: DashboardRequest, api: MonitorApi) {
  const key = JSON.stringify(request);
  const [state, setState] = useState<QueryState<DashboardSnapshot>>({ key, data: null, loading: true, error: null });
  const [retry, setRetry] = useState(0);
  const refresh = useRef<(() => void) | null>(null);
  useEffect(() => {
    let active = true;
    let busy = false;
    let lastStarted = Number.NEGATIVE_INFINITY;
    let queued: number | null = null;
    setState({ key, data: null, loading: true, error: null });
    const load = async () => {
      if (!active || busy) return;
      const remaining = 2000 - (Date.now() - lastStarted);
      if (remaining > 0) {
        if (queued === null) queued = window.setTimeout(() => { queued = null; void load(); }, remaining);
        return;
      }
      if (queued !== null) { window.clearTimeout(queued); queued = null; }
      busy = true;
      lastStarted = Date.now();
      try {
        const data = await api.dashboard(request);
        if (active) setState({ key, data, loading: false, error: null });
      } catch (error) {
        if (active) setState((previous) => ({ key, data: previous.key === key ? previous.data : null, loading: false, error: errorText(error) }));
      } finally { busy = false; }
    };
    refresh.current = () => { void load(); };
    void load();
    const timer = window.setInterval(() => { void load(); }, 2000);
    const subscription = '__TAURI_INTERNALS__' in window
      ? listen<{ revision: number }>('data_updated', () => { void load(); }).catch(() => null) : null;
    return () => {
      active = false; window.clearInterval(timer);
      if (queued !== null) window.clearTimeout(queued);
      refresh.current = null;
      void subscription?.then((unlisten) => unlisten?.());
    };
  }, [api, key, retry]); // request is fully represented by its immutable serialized key.
  return { ...(state.key === key ? state : { key, data: null, loading: true, error: null }),
    retry: () => setRetry((value) => value + 1), refresh: () => refresh.current?.() };
}

export function useTargets(request: TargetRequest | null, revision: number | undefined, api: MonitorApi) {
  const key = JSON.stringify(request);
  const [state, setState] = useState<QueryState<RankRow[]>>({ key, data: null, loading: false, error: null });
  useEffect(() => {
    let active = true;
    if (!request) { setState({ key, data: null, loading: false, error: null }); return; }
    setState((previous) => ({ key, data: previous.key === key ? previous.data : null, loading: true, error: null }));
    void api.targets(request).then((data) => {
      if (active) setState({ key, data, loading: false, error: null });
    }).catch((error: unknown) => {
      if (active) setState({ key, data: null, loading: false, error: errorText(error) });
    });
    return () => { active = false; };
  }, [api, key, revision]);
  return state.key === key ? state : { key, data: null, loading: Boolean(request), error: null };
}
