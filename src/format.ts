import type { TrafficRow } from './api';

export function totalBytes(uploaded: string, downloaded: string): bigint {
  return BigInt(uploaded) + BigInt(downloaded);
}
export function targetLabel(name: string): string {
  return !name.startsWith('[') && name.split(':').length > 2 ? `[${name}]` : name;
}
export function formatBytes(input: string | bigint): string {
  const bytes = typeof input === 'bigint' ? input : BigInt(input);
  const units = ['B', 'KiB', 'MiB', 'GiB', 'TiB', 'PiB', 'EiB'];
  let divisor = 1n;
  let index = 0;
  while (bytes >= divisor * 1024n && index < units.length - 1) { divisor *= 1024n; index++; }
  if (index === 0) return `${bytes} B`;
  const hundredths = (bytes * 100n + divisor / 2n) / divisor;
  return `${hundredths / 100n}.${String(hundredths % 100n).padStart(2, '0')} ${units[index]}`;
}
export function formatGap(seconds: number): string {
  const rounded = Math.max(1, Math.round(seconds));
  const hours = Math.floor(rounded / 3600);
  const minutes = Math.floor((rounded % 3600) / 60);
  if (hours) return `${hours} 小时 ${minutes} 分`;
  return minutes ? `${minutes} 分 ${String(rounded % 60).padStart(2, '0')} 秒` : `${rounded} 秒`;
}
export function qualityText(row: Pick<TrafficRow, 'coveredSeconds' | 'legacyCoverage' | 'samples' | 'future' | 'uploaded' | 'downloaded' | 'gapSeconds'>): string {
  if (row.future) return '该时段尚未开始';
  const total = totalBytes(row.uploaded, row.downloaded);
  let state = row.coveredSeconds > 0 ? (total === 0n ? '已采集，代理流量 0 B' : '已采集')
    : row.legacyCoverage ? '旧记录，精确覆盖未知'
      : row.samples > 0 ? '已建立采样基线' : '该时段未采集';
  if (row.coveredSeconds > 0) state += ` · 覆盖 ${(row.coveredSeconds / 60).toFixed(2)} 分钟`;
  if (row.legacyCoverage && !state.includes('覆盖未知')) state += '（含旧记录，精确覆盖未知）';
  if (row.gapSeconds > 0) state += ` · 缺口 ${formatGap(row.gapSeconds)}`;
  return state;
}
export function localDay(now = new Date()): string {
  return `${now.getFullYear()}-${String(now.getMonth() + 1).padStart(2, '0')}-${String(now.getDate()).padStart(2, '0')}`;
}
export function moveDay(day: string, offset: number): string {
  const [year, month, date] = day.split('-').map(Number);
  return localDay(new Date(year, month - 1, date + offset, 12));
}
export function clock(timestamp: number | null): string {
  return timestamp === null ? '等待首次采样' : new Date(timestamp * 1000).toLocaleTimeString('zh-CN', { hour12: false });
}
export function hourPeriod(hour: number, minute: number | null): string {
  const start = minute ?? 0;
  const end = hour * 60 + start + (minute === null ? 60 : 5);
  return `${String(hour).padStart(2, '0')}:${String(start).padStart(2, '0')}–${String(Math.floor(end / 60)).padStart(2, '0')}:${String(end % 60).padStart(2, '0')}`;
}
