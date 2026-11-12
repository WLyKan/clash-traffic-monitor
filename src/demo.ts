import type { DashboardSnapshot, MonitorApi, RankRow, Settings, TrafficRow } from './api';
import { hourPeriod, localDay, moveDay } from './format';

let revision = 0;
let settings: Settings = { alertsEnabled: false, thresholdMib: 1024, startupEnabled: false, startupAvailable: false };
const rank = (name: string, up: number, down: number): RankRow => ({ name, uploaded: String(up), downloaded: String(down) });
const row = (key: string, label: string, day: string, hour?: number): TrafficRow => ({ key, label, day, hour,
  uploaded: '0', downloaded: '0', samples: 0, coveredSeconds: 0, gapSeconds: 0, legacyCoverage: false });
function hours(day: string): TrafficRow[] {
  const now = new Date();
  return Array.from({ length: 24 }, (_, hour) => {
    const result = row(String(hour), hourPeriod(hour, null), day, hour);
    if (day === localDay() && hour > now.getHours()) return { ...result, future: true };
    if (hour === (day === localDay() ? now.getHours() : 9)) return { ...result, uploaded: String(4_000_000 + revision * 113), downloaded: String(19_000_000 + revision * 317), samples: 100, coveredSeconds: 600 + revision };
    if (hour === 1) return { ...result, samples: 20, coveredSeconds: 300 };
    if (hour === 0) return { ...result, samples: 1 };
    return result;
  });
}
function sum(rows: TrafficRow[], field: 'uploaded' | 'downloaded') { return String(rows.reduce((total, item) => total + BigInt(item[field]), 0n)); }
export const demoApi: MonitorApi = {
  async dashboard(request) {
    revision++;
    const today = localDay();
    const daily = request.trend !== 'hours';
    const now = new Date();
    const dayCount = request.trend === '7d' ? 7 : request.trend === '30d' ? 30 : request.trend === 'week' ? (now.getDay() + 6) % 7 + 1 : now.getDate();
    const days = daily ? Array.from({ length: dayCount }, (_, index) => {
      const day = moveDay(today, index - dayCount + 1);
      const values = hours(day);
      return { ...row(day, day.slice(5), day), uploaded: sum(values, 'uploaded'), downloaded: sum(values, 'downloaded'),
        samples: values.reduce((total, item) => total + item.samples, 0), coveredSeconds: values.reduce((total, item) => total + item.coveredSeconds, 0) };
    }) : [];
    const hourly = daily ? [] : hours(request.day);
    const allRows = daily ? days : hourly;
    const selected = daily ? null : hourly[request.hour];
    const scale = request.minute === null ? 1 : 0.1;
    const apps = (daily || selected && BigInt(selected.uploaded) + BigInt(selected.downloaded) > 0n)
      ? [rank('example-browser.exe', 3_000_000 * scale, 15_000_000 * scale), rank('example-editor.exe', 1_000_000 * scale, 4_000_000 * scale)] : [];
    const snapshot: DashboardSnapshot = { revision, sampledAt: Date.now() / 1000,
      status: { connected: true, message: '演示数据', lastSampleAt: Date.now() / 1000 }, days, hours: hourly, apps,
      summary: { uploaded: sum(allRows, 'uploaded'), downloaded: sum(allRows, 'downloaded'), coveredSeconds: allRows.reduce((total, item) => total + item.coveredSeconds, 0),
        gapSeconds: 0, legacyCoverage: false, unattributedUp: '0', unattributedDown: '0' }, selected,
      rangeStart: daily ? days[0].day! : request.day, rangeEnd: daily ? today : request.day, dataDir: '演示数据，无真实数据库' };
    return snapshot;
  },
  async targets(request) {
    const factor = request.hour === null ? 2 : request.minute === null ? 1 : 0.1;
    return [rank('example.com', 2_000_000 * factor, 13_000_000 * factor), rank('目标未知', 1_000_000 * factor, 2_000_000 * factor)];
  },
  async settings() { return settings; },
  async saveSettings(value) { settings = { ...value }; },
  async diagnostics() { return { mode: 'demo', writesRealDatabase: false }; },
};
