import { useEffect, useState } from 'react';
import { settingsSchema, type MonitorApi, type Settings } from './api';

export default function SettingsPanel({ api, onClose, dataDir }: { api: MonitorApi; onClose(): void; dataDir?: string }) {
  const [settings, setSettings] = useState<Settings | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const [message, setMessage] = useState('');
  const [diagnostics, setDiagnostics] = useState<string | null>(null);
  const [load, setLoad] = useState(0);
  useEffect(() => {
    let active = true;
    void api.settings().then((value) => { if (active) { setSettings(value); setError(null); } })
      .catch((reason: unknown) => { if (active) setError(String(reason)); });
    return () => { active = false; };
  }, [api, load]);

  const save = async () => {
    if (!settings) return;
    const checked = settingsSchema.safeParse(settings);
    if (!checked.success) { setError('请输入 1–1,000,000 的整数 MiB 阈值。'); return; }
    setSaving(true); setMessage(''); setError(null);
    try { await api.saveSettings(checked.data); setMessage('设置已保存'); }
    catch (reason) { setError(String(reason)); }
    finally { setSaving(false); }
  };
  return <section className="settings-panel" aria-label="设置">
    <div className="section-heading"><h2>设置</h2><button type="button" className="text-button" onClick={onClose}>收起设置</button></div>
    {error && <div className="error-banner" role="alert">{error} {!settings && <button onClick={() => setLoad((value) => value + 1)}>重试</button>}</div>}
    {!settings ? <p className="empty-state">正在读取设置…</p> : <form onSubmit={(event) => { event.preventDefault(); void save(); }}>
      <div className="settings-grid">
        <label className="setting-line"><input type="checkbox" checked={settings.alertsEnabled} onChange={(event) => setSettings({ ...settings, alertsEnabled: event.target.checked })} />
          <span><strong>代理流量突增提醒</strong><small>连续采集 15 分钟后检测，每 15 分钟最多提醒一次。</small></span></label>
        <label className="threshold-label">15 分钟阈值 <span><input aria-label="提醒阈值 MiB" type="number" min="1" max="1000000" step="1" required value={Number.isFinite(settings.thresholdMib) ? settings.thresholdMib : ''}
          onChange={(event) => setSettings({ ...settings, thresholdMib: event.target.valueAsNumber })} /> MiB</span></label>
        <label className="setting-line"><input type="checkbox" disabled={!settings.startupAvailable} checked={settings.startupEnabled}
          onChange={(event) => setSettings({ ...settings, startupEnabled: event.target.checked })} />
          <span><strong>登录 Windows 后收起到托盘</strong><small>{settings.startupAvailable ? '仅为当前 Windows 用户设置，保留现有数据目录。' : '独立数据目录下不提供登录自启。'}</small></span></label>
      </div>
      <div className="settings-actions"><button className="primary-button" type="submit" disabled={saving}>{saving ? '正在保存…' : '保存设置'}</button><span role="status">{message}</span></div>
    </form>}
    {dataDir && <p className="data-location">数据保存位置 <span>{dataDir}</span></p>}
    <button className="text-button" type="button" onClick={() => { void api.diagnostics().then((result) => setDiagnostics(JSON.stringify(result, null, 2))).catch((reason: unknown) => setError(String(reason))); }}>查看采集诊断</button>
    {diagnostics && <pre className="diagnostics" tabIndex={0}>{diagnostics}</pre>}
  </section>;
}
