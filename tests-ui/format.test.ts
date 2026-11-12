import { describe, expect, it } from 'vitest';
import { formatBytes, hourPeriod, moveDay, qualityText, targetLabel, totalBytes } from '../src/format';
import { dashboardSchema } from '../src/api';
import { row, snapshot } from './fixtures';

describe('lossless IPC bytes and coverage language', () => {
  it('adds values beyond Number precision without rounding the source', () => {
    expect(totalBytes('9007199254740993', '7')).toBe(9007199254741000n);
    expect(formatBytes('0')).toBe('0 B');
    expect(formatBytes('1024')).toBe('1.00 KiB');
    expect(formatBytes('18446744073709551615')).toBe('16.00 EiB');
  });
  it('distinguishes missing, baseline, collected zero, legacy, future and gaps', () => {
    expect(qualityText(row())).toBe('该时段未采集');
    expect(qualityText(row({ samples: 1 }))).toBe('已建立采样基线');
    expect(qualityText(row({ coveredSeconds: 120 }))).toContain('已采集，代理流量 0 B');
    expect(qualityText(row({ legacyCoverage: true }))).toBe('旧记录，精确覆盖未知');
    expect(qualityText(row({ coveredSeconds: 60, legacyCoverage: true, gapSeconds: 5 }))).toContain('含旧记录，精确覆盖未知');
    expect(qualityText(row({ gapSeconds: 65 }))).toContain('缺口 1 分 05 秒');
    expect(qualityText(row({ future: true }))).toBe('该时段尚未开始');
  });
  it('keeps end-of-day interval and leap-day navigation explicit', () => {
    expect(hourPeriod(23, 55)).toBe('23:55–24:00');
    expect(hourPeriod(23, null)).toBe('23:00–24:00');
    expect(moveDay('2024-02-28', 1)).toBe('2024-02-29');
  });
  it('distinguishes IPv6 from IPv4 or domain with a port', () => {
    expect(targetLabel('2001:db8::1')).toBe('[2001:db8::1]');
    expect(targetLabel('[2001:db8::1]:443')).toBe('[2001:db8::1]:443');
    expect(targetLabel('192.0.2.1:443')).toBe('192.0.2.1:443');
    expect(targetLabel('example.com:443')).toBe('example.com:443');
  });
  it('rejects malformed backend byte fields at the IPC boundary', () => {
    const data = snapshot({ day: '2026-11-01', trend: 'hours', hour: 0, minute: null });
    expect(dashboardSchema.safeParse(data).success).toBe(true);
    expect(dashboardSchema.safeParse({ ...data, summary: { ...data.summary, uploaded: 12 } }).success).toBe(false);
  });
});
