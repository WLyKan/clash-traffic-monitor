import { useEffect, useRef, useState } from 'react';
import { listen } from '@tauri-apps/api/event';
import { monitorApi, type DashboardRequest, type MonitorApi, type RankRow, type TargetRequest, type TrafficRow, type Trend } from './api';
import TrendChart from './TrendChart';
import SettingsPanel from './SettingsPanel';
import { clock, formatBytes, formatGap, hourPeriod, localDay, moveDay, qualityText, targetLabel, totalBytes } from './format';
import { useDashboard, useTargets } from './useMonitor';
import { demoApi } from './demo';

const trendLabels: ReadonlyArray<readonly [Trend, string]> = [
  ['hours', '24 小时'], ['7d', '最近 7 天'], ['30d', '最近 30 天'], ['week', '本周'], ['month', '本月'],
];
const isTauri = () => '__TAURI_INTERNALS__' in window;
const demoRequested = import.meta.env.DEV && new URLSearchParams(window.location.search).get('demo') === '1';

function Arrow({ direction }: { direction: 'left' | 'right' }) {
  return <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.7" aria-hidden="true"><path d={direction === 'left' ? 'm14 6-6 6 6 6' : 'm10 6 6 6-6 6'} /></svg>;
}
function SignalIcon() {
  return <svg width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.7" aria-hidden="true"><path d="M4 18V12m6 6V6m6 12V9m4 12H2" strokeLinecap="round" /></svg>;
}
function SettingsIcon() {
  return <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.6" aria-hidden="true"><path d="M4 7h16M4 17h16M8 4v6m8 4v6" strokeLinecap="round" /></svg>;
}

function RankTable({ rows, kind, selectedApp, onSelect }: { rows: RankRow[]; kind: 'apps' | 'targets'; selectedApp?: string | null; onSelect?(name: string): void }) {
  return <div className="table-scroll"><table className="rank-table" aria-label={kind === 'apps' ? '应用流量排行' : '访问目标流量排行'}>
    <thead><tr><th className="rank-column">排名</th><th>{kind === 'apps' ? '应用' : '目标'}</th><th>下载</th><th>上传</th><th>合计</th></tr></thead>
    <tbody>{rows.map((row, index) => {
      const name = kind === 'apps' ? row.name || '未知应用' : targetLabel(row.name);
      return <tr key={row.name} className={kind === 'apps' && selectedApp === row.name ? 'selected-row' : undefined}>
        <td className="rank-number">{index + 1}</td>
        <th scope="row">{onSelect ? <button className="app-select" aria-pressed={selectedApp === row.name} onClick={() => onSelect(row.name)}>{name}</button> : <span className="target-name" title={name}>{name}</span>}</th>
        <td>{formatBytes(row.downloaded)}</td><td>{formatBytes(row.uploaded)}</td><td className="total-cell">{formatBytes(totalBytes(row.uploaded, row.downloaded))}</td>
      </tr>;
    })}</tbody>
  </table></div>;
}

export default function App({ api: providedApi }: { api?: MonitorApi }) {
  const api = providedApi ?? (demoRequested ? demoApi : monitorApi);
  const [request, setRequest] = useState<DashboardRequest>(() => ({ day: localDay(), trend: 'hours', hour: new Date().getHours(), minute: null }));
  const [dateInput, setDateInput] = useState(request.day);
  const [dateError, setDateError] = useState('');
  const [selectedApp, setSelectedApp] = useState<string | null>(null);
  const [targetScope, setTargetScope] = useState<'period' | 'day'>('period');
  const [showSettings, setShowSettings] = useState(false);
  const navigation = useRef(0);
  const dashboard = useDashboard(request, api);
  const data = dashboard.data;
  const daily = request.trend !== 'hours';
  const rows = data ? daily ? data.days : data.hours : [];
  const selected = data?.selected;
  const targetRequest: TargetRequest | null = !daily && selectedApp !== null
    ? { day: request.day, app: selectedApp, hour: targetScope === 'day' ? null : request.hour,
      minute: targetScope === 'day' ? null : request.minute } : null;
  const targets = useTargets(targetRequest, data?.revision, api);

  const selectDate = (day: string) => {
    if (!/^\d{4}-\d{2}-\d{2}$/.test(day) || localDay(new Date(`${day}T12:00:00`)) !== day || day > localDay()) {
      setDateError('请选择今天或更早的有效日期。'); return;
    }
    const generation = ++navigation.current;
    const base: DashboardRequest = { day, trend: 'hours', hour: day === localDay() ? new Date().getHours() : 0, minute: null };
    setDateError(''); setDateInput(day); setRequest(base);
    // Historical-date navigation follows the incumbent highest-traffic-hour selection.
    if (day < localDay()) void api.dashboard(base).then((snapshot) => {
      if (generation !== navigation.current || !snapshot.hours.length) return;
      const peak = snapshot.hours.reduce((best, row) => {
        const left = totalBytes(best.uploaded, best.downloaded);
        const right = totalBytes(row.uploaded, row.downloaded);
        return right > left || (right === left && row.samples > best.samples) ? row : best;
      });
      setRequest({ ...base, hour: peak.hour ?? Number(peak.key) });
    }).catch(() => { /* The main query exposes the actionable read failure. */ });
  };
  const selectHour = (hour: number) => { navigation.current++; setRequest({ ...request, hour, minute: null }); };
  const selectTrend = (trend: Trend) => { navigation.current++; setRequest({ ...request, trend, minute: null }); };
  const selectChart = (row: TrafficRow) => {
    if (daily) selectDate(row.day ?? row.key);
    else selectHour(row.hour ?? Number(row.key));
  };

  useEffect(() => {
    if (!isTauri()) return;
    let active = true;
    const subscription = listen<{ peakAt: number }>('open-alert', (event) => {
      if (!active || typeof event.payload.peakAt !== 'number') return;
      const date = new Date(event.payload.peakAt * 1000);
      navigation.current++;
      const day = localDay(date);
      setDateInput(day); setDateError('');
      setRequest({ day, trend: 'hours', hour: date.getHours(), minute: Math.floor(date.getMinutes() / 5) * 5 });
      setTargetScope('period');
    }).catch(() => null);
    return () => { active = false; void subscription.then((unlisten) => unlisten?.()); };
  }, []);

  const summary = data?.summary;
  const selectedQuality = selected ? qualityText(selected) : '读取时段数据中…';
  return <main className="app-shell mx-auto w-full">
    <header className="app-header flex items-center justify-between gap-4">
      <div className="app-identity flex items-center gap-3"><span className="app-symbol"><SignalIcon /></span><div><h1>Clash 流量监控</h1><p>代理用量与访问目标</p></div></div>
      <div className="header-actions flex items-center gap-3"><div className={`collector-status ${data?.status.connected ? 'is-online' : 'is-offline'}`} role="status">
        <span className="status-dot" /><span>{data ? data.status.connected ? '正在采集' : '采集离线' : '正在连接'}<small>{clock(data?.status.lastSampleAt ?? null)}</small></span>
      </div><button className={`icon-label-button ${showSettings ? 'is-active' : ''}`} aria-expanded={showSettings} onClick={() => setShowSettings(!showSettings)}><SettingsIcon />设置</button></div>
    </header>
    {demoRequested && !providedApi && <div className="demo-banner" role="note">演示数据 · 当前为浏览器预览，不会连接 Mihomo 或写入真实数据库。</div>}
    {showSettings && <SettingsPanel api={api} onClose={() => setShowSettings(false)} dataDir={data?.dataDir} />}
    <section className="usage-panel" aria-label="代理流量概览">
      <div className="view-toolbar flex flex-wrap items-center justify-between gap-3">
        <div className="segmented-control" aria-label="趋势范围">{trendLabels.map(([key, label]) => <button key={key} type="button" aria-pressed={request.trend === key} className={request.trend === key ? 'is-active' : ''} onClick={() => selectTrend(key)}>{label}</button>)}</div>
        <form className="date-controls flex items-center gap-2" onSubmit={(event) => { event.preventDefault(); selectDate(dateInput); }}>
          <button type="button" className="icon-button" aria-label="上一天" onClick={() => selectDate(moveDay(request.day, -1))}><Arrow direction="left" /></button>
          <label className="sr-only" htmlFor="selected-date">日期</label><input id="selected-date" type="date" max={localDay()} value={dateInput} onChange={(event) => setDateInput(event.target.value)} aria-invalid={Boolean(dateError)} />
          <button type="submit" className="subtle-button">查看</button>
          <button type="button" className="icon-button" aria-label="下一天" disabled={request.day >= localDay()} onClick={() => selectDate(moveDay(request.day, 1))}><Arrow direction="right" /></button>
          <button type="button" className="text-button" onClick={() => selectDate(localDay())}>今天</button>
        </form>
      </div>
      {dateError && <p className="inline-error" role="alert">{dateError}</p>}
      <div className="overview-line flex flex-wrap items-end justify-between gap-6">
        <div className="usage-title"><h2>{daily ? `${data?.rangeStart ?? ''} — ${data?.rangeEnd ?? ''}` : request.day}</h2><p>{daily ? '范围代理累计' : '当日代理用量'}</p></div>
        <dl className="traffic-totals flex items-end gap-8">
          <div><dt>代理合计</dt><dd>{summary ? formatBytes(totalBytes(summary.uploaded, summary.downloaded)) : '—'}</dd></div>
          <div className="secondary-total"><dt><i className="legend-dot download" />下载</dt><dd>{summary ? formatBytes(summary.downloaded) : '—'}</dd></div>
          <div className="secondary-total"><dt><i className="legend-dot upload" />上传</dt><dd>{summary ? formatBytes(summary.uploaded) : '—'}</dd></div>
        </dl>
      </div>
      {dashboard.error && <div className="error-banner" role="alert"><span>暂时无法读取本地流量：{dashboard.error}</span><button onClick={dashboard.retry}>重新读取</button></div>}
      {!data && dashboard.loading ? <div className="chart-loading" role="status">正在读取流量记录…</div>
        : <TrendChart key={`${request.trend}:${request.day}`} rows={rows} selectedKey={daily ? undefined : String(request.hour)} daily={daily} onSelect={selectChart} />}
      <div className="quality-line flex flex-wrap items-center justify-between gap-2">
        <p>{summary ? <>采集覆盖 <b>{(summary.coveredSeconds / 60).toFixed(2)}</b> 分钟{summary.legacyCoverage && '（含旧记录，精确覆盖未知）'}{summary.gapSeconds > 0 && <> · 缺口 {formatGap(summary.gapSeconds)}</>}</> : '历史从首次成功采集起算'}</p>
        <span className="quality-legend"><i className="gap-dot" />采集缺口</span>
      </div>
    </section>
    {data && !data.status.connected && <div className="offline-note" role="note">{data.status.message || '暂时无法连接 Mihomo，请确认 Clash Verge 已启动。'} 历史查询仍可使用。</div>}
    <div className="ranking-grid">
      <section className="ranking-panel" aria-label="应用排行">
        <div className="section-heading"><div><h2>{daily ? '范围应用排行' : '应用排行'}</h2><p>{daily ? '选中日期后可查看小时与目标' : `${hourPeriod(request.hour, request.minute)} · ${data?.apps.length ?? 0} 项`}</p></div>
          <div className="period-controls flex items-center gap-2"><label><span className="sr-only">小时</span><select aria-label="小时" disabled={daily} value={request.hour} onChange={(event) => selectHour(Number(event.target.value))}>{Array.from({ length: 24 }, (_, hour) => <option value={hour} key={hour}>{String(hour).padStart(2, '0')}:00</option>)}</select></label>
            <label><span className="sr-only">五分钟区间</span><select aria-label="五分钟区间" disabled={daily} value={request.minute ?? 'hour'} onChange={(event) => { navigation.current++; setRequest({ ...request, minute: event.target.value === 'hour' ? null : Number(event.target.value) }); }}><option value="hour">整小时</option>{Array.from({ length: 12 }, (_, index) => <option key={index} value={index * 5}>{String(index * 5).padStart(2, '0')}–{String(index * 5 + 5).padStart(2, '0')} 分</option>)}</select></label></div>
        </div>
        {!daily && <p className="period-quality">{selectedQuality}</p>}
        {data?.apps.length ? <RankTable rows={data.apps} kind="apps" selectedApp={selectedApp} onSelect={setSelectedApp} /> : <div className="empty-state">{dashboard.loading ? '正在读取应用…' : daily ? '此范围暂无已确认代理流量。' : selectedQuality}</div>}
      </section>
      <section className="ranking-panel target-panel" aria-label="访问目标排行">
        <div className="section-heading"><div><h2>访问目标排行</h2><p>{selectedApp === null ? '选择应用，了解流量去向' : `${selectedApp || '未知应用'} · ${targetScope === 'day' ? '所选日期整日' : hourPeriod(request.hour, request.minute)}`}</p></div>
          <select aria-label="目标排行范围" disabled={daily || selectedApp === null} value={targetScope} onChange={(event) => setTargetScope(event.target.value as 'period' | 'day')}><option value="period">所选时段</option><option value="day">所选日期整日</option></select>
        </div>
        {targets.error ? <div className="error-banner" role="alert">暂时无法读取访问目标：{targets.error}</div> : !targetRequest ? <div className="empty-state">{daily ? '请先选择图表中的日期，再选择应用。' : '在应用排行中选择一个应用。'}</div>
          : targets.data?.length ? <RankTable rows={targets.data} kind="targets" /> : <div className="empty-state">{targets.loading ? '正在读取目标…' : '该应用在所选时段没有目标流量记录。'}</div>}
      </section>
    </div>
    <footer className="app-footer flex flex-wrap items-center justify-between gap-2"><span>关闭窗口后继续在托盘采集</span><span>{summary && !daily ? `未归因流量 ${formatBytes(totalBytes(summary.unattributedUp, summary.unattributedDown))}，可能包含直连` : '仅统计已确认的代理流量'}</span></footer>
  </main>;
}
