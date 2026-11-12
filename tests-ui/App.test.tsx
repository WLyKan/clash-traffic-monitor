import { act, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import type { Event } from '@tauri-apps/api/event';
import type { DashboardRequest, MonitorApi } from '../src/api';
const native = vi.hoisted(() => ({ handler: null as ((event: Event<{ peakAt: number }>) => void) | null, unlisten: vi.fn() }));
vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn(async (event, handler) => { if (event === 'open-alert') native.handler = handler; return native.unlisten; }) }));
vi.mock('echarts/core', () => ({ use: vi.fn(), init: () => ({ setOption: vi.fn(), resize: vi.fn(), dispose: vi.fn() }) }));
import App from '../src/App';
import SettingsPanel from '../src/SettingsPanel';
import { fakeApi, snapshot } from './fixtures';
import { localDay } from '../src/format';

afterEach(() => { Reflect.deleteProperty(window, '__TAURI_INTERNALS__'); native.handler = null; });
function lastDashboardRequest(api: MonitorApi): DashboardRequest { return vi.mocked(api.dashboard).mock.calls.at(-1)![0]; }

describe('dashboard drilldown and native integration', () => {
  it('links historical day, peak hour, five-minute selection and target period/day scopes', async () => {
    const api = fakeApi();
    render(<App api={api} />);
    await screen.findByRole('button', { name: 'browser.exe' });
    fireEvent.change(screen.getByLabelText('日期'), { target: { value: '2026-11-01' } });
    fireEvent.click(screen.getByRole('button', { name: '查看' }));
    await waitFor(() => expect(screen.getByRole('combobox', { name: '小时' })).toHaveValue('9'));
    expect(lastDashboardRequest(api).day).toBe('2026-11-01');
    fireEvent.change(screen.getByRole('combobox', { name: '小时' }), { target: { value: '10' } });
    fireEvent.change(screen.getByRole('combobox', { name: '五分钟区间' }), { target: { value: '15' } });
    await waitFor(() => expect(lastDashboardRequest(api)).toMatchObject({ hour: 10, minute: 15 }));
    await screen.findByRole('button', { name: 'browser.exe' });
    fireEvent.click(screen.getByRole('button', { name: 'browser.exe' }));
    expect(await screen.findByText('example.com')).toBeInTheDocument();
    expect(api.targets).toHaveBeenLastCalledWith({ day: '2026-11-01', app: 'browser.exe', hour: 10, minute: 15 });
    fireEvent.change(screen.getByRole('combobox', { name: '目标排行范围' }), { target: { value: 'day' } });
    await waitFor(() => expect(api.targets).toHaveBeenLastCalledWith({ day: '2026-11-01', app: 'browser.exe', hour: null, minute: null }));
  });
  it('clears hover when switching trend and drills daily empty columns into hours', async () => {
    const api = fakeApi();
    render(<App api={api} />);
    await screen.findByTestId('column-0');
    expect(screen.getByRole('contentinfo')).toHaveTextContent('未归因流量 0 B');
    fireEvent.pointerEnter(screen.getByTestId('column-0'));
    expect(screen.getByRole('tooltip')).toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: '最近 7 天' }));
    await screen.findByTestId('column-2026-11-01');
    expect(screen.getByRole('contentinfo')).toHaveTextContent('仅统计已确认的代理流量');
    expect(screen.getByRole('contentinfo')).not.toHaveTextContent('未归因流量');
    expect(screen.queryByRole('tooltip')).not.toBeInTheDocument();
    expect(screen.getByRole('combobox', { name: '小时' })).toBeDisabled();
    fireEvent.click(screen.getByTestId('column-2026-11-01'));
    await waitFor(() => expect(lastDashboardRequest(api)).toMatchObject({ day: '2026-11-01', trend: 'hours', hour: 9 }));
    expect(screen.getByRole('combobox', { name: '小时' })).toBeEnabled();
  });
  it('opens the native alert peak in its exact five-minute interval', async () => {
    Object.defineProperty(window, '__TAURI_INTERNALS__', { configurable: true, value: {} });
    const api = fakeApi();
    const { unmount } = render(<App api={api} />);
    await waitFor(() => expect(native.handler).not.toBeNull());
    const peak = new Date(2026, 10, 1, 17, 18, 59);
    act(() => native.handler!({ event: 'open-alert', id: 1, payload: { peakAt: peak.getTime() / 1000 } }));
    await waitFor(() => expect(lastDashboardRequest(api)).toEqual({ day: localDay(peak), trend: 'hours', hour: 17, minute: 15 }));
    expect(screen.getByRole('combobox', { name: '五分钟区间' })).toHaveValue('15');
    unmount();
    await waitFor(() => expect(native.unlisten).toHaveBeenCalled());
  });
  it('shows offline history and provides retry after a failed initial read', async () => {
    const api = fakeApi({ dashboard: vi.fn().mockRejectedValueOnce(new Error('database busy')).mockImplementation(async (request: DashboardRequest) => ({ ...snapshot(request), status: { connected: false, message: 'Mihomo 未连接', lastSampleAt: 1000 } })) });
    render(<App api={api} />);
    expect(await screen.findByRole('alert')).toHaveTextContent('database busy');
    fireEvent.click(screen.getByRole('button', { name: '重新读取' }));
    expect(await screen.findByText(/历史查询仍可使用/)).toHaveTextContent('Mihomo 未连接');
    expect(screen.getByRole('button', { name: 'browser.exe' })).toBeInTheDocument();
  });
});

describe('settings failure and recovery', () => {
  it('keeps edited settings after a failed save and saves the identical draft on retry', async () => {
    const api = fakeApi({ saveSettings: vi.fn().mockRejectedValueOnce(new Error('startup permission denied')).mockResolvedValue(undefined) });
    render(<SettingsPanel api={api} onClose={vi.fn()} dataDir="D:/isolated" />);
    const threshold = await screen.findByRole('spinbutton', { name: '提醒阈值 MiB' });
    fireEvent.change(threshold, { target: { value: '750' } });
    fireEvent.click(screen.getByRole('checkbox', { name: /登录 Windows/ }));
    fireEvent.click(screen.getByRole('button', { name: '保存设置' }));
    expect(await screen.findByRole('alert')).toHaveTextContent('startup permission denied');
    expect(threshold).toHaveValue(750);
    expect(screen.getByRole('checkbox', { name: /登录 Windows/ })).toBeChecked();
    fireEvent.click(screen.getByRole('button', { name: '保存设置' }));
    expect(await screen.findByText('设置已保存')).toBeInTheDocument();
    expect(api.saveSettings).toHaveBeenLastCalledWith({ alertsEnabled: true, thresholdMib: 750, startupEnabled: true, startupAvailable: true });
    const section = screen.getByRole('region', { name: '设置' });
    expect(within(section).getByText('D:/isolated')).toBeInTheDocument();
  });
});
