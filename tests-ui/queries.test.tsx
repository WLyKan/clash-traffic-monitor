import { act, renderHook, waitFor } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
const native = vi.hoisted(() => ({ changed: null as (() => void) | null, unlisten: vi.fn() }));
vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn(async (_event, handler) => { native.changed = handler; return native.unlisten; }) }));
import { useDashboard, useTargets } from '../src/useMonitor';
import type { DashboardRequest, DashboardSnapshot, RankRow } from '../src/api';
import { deferred, fakeApi, snapshot } from './fixtures';

const initial: DashboardRequest = { day: '2026-11-01', trend: 'hours', hour: 0, minute: null };
afterEach(() => { vi.useRealTimers(); Reflect.deleteProperty(window, '__TAURI_INTERNALS__'); native.changed = null; });
describe('query lifecycle', () => {
  it('ignores a late previous-date response', async () => {
    const old = deferred<DashboardSnapshot>();
    const current = { ...initial, day: '2026-11-02' };
    const api = fakeApi({ dashboard: vi.fn((request) => request.day === initial.day ? old.promise : Promise.resolve(snapshot(request))) });
    const { result, rerender } = renderHook(({ request }) => useDashboard(request, api), { initialProps: { request: initial } });
    rerender({ request: current });
    await waitFor(() => expect(result.current.data?.rangeEnd).toBe(current.day));
    await act(async () => { old.resolve(snapshot(initial)); });
    expect(result.current.data?.rangeEnd).toBe(current.day);
  });
  it('polls after two seconds without overlapping slow reads', async () => {
    vi.useFakeTimers();
    const slow = deferred<DashboardSnapshot>();
    const read = vi.fn().mockReturnValueOnce(slow.promise).mockResolvedValue(snapshot(initial));
    const api = fakeApi({ dashboard: read });
    const { result } = renderHook(() => useDashboard(initial, api));
    await act(async () => { vi.advanceTimersByTime(4000); });
    expect(read).toHaveBeenCalledTimes(1);
    await act(async () => { slow.resolve(snapshot(initial)); });
    await act(async () => { vi.advanceTimersByTime(2000); });
    expect(read).toHaveBeenCalledTimes(2);
    expect(result.current.data).not.toBeNull();
  });
  it('never puts a late target result under a newly selected app', async () => {
    const old = deferred<RankRow[]>();
    const api = fakeApi({ targets: vi.fn((request) => request.app === 'old' ? old.promise : Promise.resolve([{ name: 'new.example', uploaded: '0', downloaded: '10' }])) });
    const first = { day: initial.day, app: 'old', hour: 0, minute: null };
    const { result, rerender } = renderHook(({ request }) => useTargets(request, 1, api), { initialProps: { request: first } });
    rerender({ request: { ...first, app: 'new' } });
    await waitFor(() => expect(result.current.data?.[0].name).toBe('new.example'));
    await act(async () => { old.resolve([{ name: 'old.example', uploaded: '0', downloaded: '1' }]); });
    expect(result.current.data?.[0].name).toBe('new.example');
  });
  it('coalesces native updates and polling into one serial read per two seconds', async () => {
    vi.useFakeTimers();
    Object.defineProperty(window, '__TAURI_INTERNALS__', { configurable: true, value: {} });
    const api = fakeApi();
    const { unmount } = renderHook(() => useDashboard(initial, api));
    await act(async () => {});
    expect(api.dashboard).toHaveBeenCalledTimes(1);
    await act(async () => { vi.advanceTimersByTime(500); native.changed!(); native.changed!(); });
    expect(api.dashboard).toHaveBeenCalledTimes(1);
    await act(async () => { vi.advanceTimersByTime(1500); });
    expect(api.dashboard).toHaveBeenCalledTimes(2);
    await act(async () => { native.changed!(); native.changed!(); vi.advanceTimersByTime(2000); });
    expect(api.dashboard).toHaveBeenCalledTimes(3);
    unmount();
    await act(async () => { vi.advanceTimersByTime(4000); });
    expect(api.dashboard).toHaveBeenCalledTimes(3);
    expect(native.unlisten).toHaveBeenCalled();
  });
});
