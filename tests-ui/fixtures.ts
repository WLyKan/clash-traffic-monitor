import { vi } from 'vitest';
import type { DashboardRequest, DashboardSnapshot, MonitorApi, TrafficRow } from '../src/api';
import { hourPeriod } from '../src/format';

export function row(overrides: Partial<TrafficRow> = {}): TrafficRow {
  return { key: '0', label: '00:00–01:00', hour: 0, day: '2026-11-01', uploaded: '0', downloaded: '0',
    samples: 0, coveredSeconds: 0, gapSeconds: 0, legacyCoverage: false, ...overrides };
}
export function snapshot(request: DashboardRequest): DashboardSnapshot {
  const hours = Array.from({ length: 24 }, (_, hour) => row({ key: String(hour), hour,
    day: request.day, label: hourPeriod(hour, null), downloaded: hour === 9 ? '8192' : '0',
    coveredSeconds: hour === 9 ? 300 : 0, samples: hour === 9 ? 150 : 0 }));
  return { revision: 1, sampledAt: 1000, status: { connected: true, message: '', lastSampleAt: 1000 },
    days: [row({ key: '2026-11-01', day: '2026-11-01', label: '11-01', downloaded: '8192', coveredSeconds: 300 })],
    hours, apps: [{ name: 'browser.exe', uploaded: '512', downloaded: '8192' }],
    summary: { uploaded: '512', downloaded: '8192', coveredSeconds: 300, gapSeconds: 0, legacyCoverage: false,
      unattributedUp: '0', unattributedDown: '0' }, selected: hours[request.hour],
    rangeStart: '2026-11-01', rangeEnd: request.day, dataDir: 'D:\\isolated-test-data' };
}
export function fakeApi(overrides: Partial<MonitorApi> = {}): MonitorApi {
  return { dashboard: vi.fn(async (request) => snapshot(request)),
    targets: vi.fn(async () => [{ name: 'example.com', uploaded: '512', downloaded: '8192' }]),
    settings: vi.fn(async () => ({ alertsEnabled: true, thresholdMib: 500, startupEnabled: false, startupAvailable: true })),
    saveSettings: vi.fn(async () => {}), diagnostics: vi.fn(async () => ({ isolated: true })), ...overrides };
}
export function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason: unknown) => void;
  const promise = new Promise<T>((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}
