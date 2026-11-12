use crate::paths::{default_data_dir, resolve_data_dir, WriterLock};
use chrono::{Local, NaiveDate, Timelike};
use monitor_core::{
    time::unix_now, AlertSettings, MihomoSource, SamplingState, SpikeAlert, SpikeDetector,
};
use monitor_store::{trend_start, AppTraffic, DayTraffic, HourTraffic, Store};
use serde::{Deserialize, Serialize};
use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc, Arc, Mutex,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub connected: bool,
    pub message: String,
    pub last_sample_at: Option<f64>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    pub alerts_enabled: bool,
    pub threshold_mib: u64,
    pub startup_enabled: bool,
    pub startup_available: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DashboardRequest {
    pub day: String,
    pub trend: String,
    pub hour: u32,
    pub minute: Option<u32>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TargetRequest {
    pub day: String,
    pub app: String,
    pub hour: Option<u32>,
    pub minute: Option<u32>,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrafficRow {
    pub key: String,
    pub label: String,
    pub day: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hour: Option<u32>,
    pub uploaded: String,
    pub downloaded: String,
    pub samples: i64,
    pub covered_seconds: f64,
    pub gap_seconds: f64,
    pub legacy_coverage: bool,
    pub future: bool,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RankRow {
    pub name: String,
    pub uploaded: String,
    pub downloaded: String,
}
impl From<AppTraffic> for RankRow {
    fn from(row: AppTraffic) -> Self {
        Self {
            name: row.app.unwrap_or_default(),
            uploaded: row.uploaded.to_string(),
            downloaded: row.downloaded.to_string(),
        }
    }
}
#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Summary {
    pub uploaded: String,
    pub downloaded: String,
    pub covered_seconds: f64,
    pub gap_seconds: f64,
    pub legacy_coverage: bool,
    pub unattributed_up: String,
    pub unattributed_down: String,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DashboardSnapshot {
    pub revision: u64,
    pub sampled_at: Option<f64>,
    pub status: Status,
    pub days: Vec<TrafficRow>,
    pub hours: Vec<TrafficRow>,
    pub apps: Vec<RankRow>,
    pub summary: Summary,
    pub selected: Option<TrafficRow>,
    pub range_start: String,
    pub range_end: String,
    pub data_dir: String,
}
enum Control {
    Settings(AlertSettings, mpsc::Sender<Result<(), String>>),
}
pub struct Runtime {
    pub directory: PathBuf,
    store: Mutex<Store>,
    status: Mutex<Status>,
    settings: Mutex<AlertSettings>,
    revision: AtomicU64,
    stop: AtomicBool,
    control: mpsc::Sender<Control>,
    worker: Mutex<Option<JoinHandle<()>>>,
    pending: Mutex<Vec<SpikeAlert>>,
    latest: Mutex<Option<SpikeAlert>>,
    _lock: WriterLock,
}
impl Runtime {
    pub fn start(directory: PathBuf) -> Result<Arc<Self>, String> {
        Self::start_with_source(directory, MihomoSource::from_env())
    }
    /// Dependency boundary used by isolated controllers and runtime integration
    /// tests. The production constructor always discovers the real transport.
    pub fn start_with_source(
        directory: PathBuf,
        source: Result<MihomoSource, String>,
    ) -> Result<Arc<Self>, String> {
        let lock = WriterLock::acquire(&directory)?;
        let store = Store::open(directory.join("traffic.sqlite3")).map_err(|e| e.to_string())?;
        let last = store.last_sample().map_err(|e| e.to_string())?;
        let settings = store
            .load_alert_settings()
            .map_err(|e| e.to_string())?
            .unwrap_or_default();
        let (tx, rx) = mpsc::channel();
        let runtime = Arc::new(Self {
            directory,
            store: Mutex::new(store),
            status: Mutex::new(Status {
                connected: false,
                message: "正在连接 Mihomo".into(),
                last_sample_at: last,
            }),
            settings: Mutex::new(settings.clone()),
            revision: AtomicU64::new(0),
            stop: AtomicBool::new(false),
            control: tx,
            worker: Mutex::new(None),
            pending: Mutex::new(Vec::new()),
            latest: Mutex::new(None),
            _lock: lock,
        });
        let weak = Arc::downgrade(&runtime);
        let handle = thread::Builder::new()
            .name("traffic-collector".into())
            .spawn(move || {
                let mut source = source;
                let mut sampling = SamplingState::new(last, 1.0);
                let mut detector = SpikeDetector::new(settings);
                loop {
                    let Some(runtime) = weak.upgrade() else { break };
                    if runtime.stop.load(Ordering::Acquire) {
                        break;
                    }
                    let started = Instant::now();
                    while let Ok(Control::Settings(settings, reply)) = rx.try_recv() {
                        let result = runtime.apply_alert_settings(settings, &mut detector);
                        let _ = reply.send(result);
                    }
                    let result = match source.as_mut() {
                        Ok(client) => client.sample(),
                        Err(message) => Err(message.clone()),
                    };
                    match result {
                        Ok(current) => {
                            let record = sampling.prepare(&current);
                            let result = runtime
                                .store
                                .lock()
                                .unwrap()
                                .record(&record)
                                .map_err(|e| e.to_string());
                            match result {
                                Ok(()) => {
                                    if let Some(from) = record.covered_from {
                                        if let Some(alert) =
                                            detector.add(from, current.sampled_at, &record.apps)
                                        {
                                            *runtime.latest.lock().unwrap() = Some(alert.clone());
                                            runtime.pending.lock().unwrap().push(alert);
                                        }
                                    } else {
                                        detector.reset();
                                    }
                                    *runtime.status.lock().unwrap() = Status {
                                        connected: true,
                                        message: "正在采集代理流量".into(),
                                        last_sample_at: Some(current.sampled_at),
                                    };
                                    sampling.commit(current, &record);
                                }
                                Err(message) => {
                                    sampling.disconnected();
                                    detector.reset();
                                    let mut status = runtime.status.lock().unwrap();
                                    status.connected = false;
                                    status.message = format!("无法写入本地流量数据库：{message}");
                                }
                            }
                        }
                        Err(message) => {
                            sampling.disconnected();
                            detector.reset();
                            let mut status = runtime.status.lock().unwrap();
                            status.connected = false;
                            status.message = message;
                            drop(status);
                            if source.is_err() {
                                source = MihomoSource::from_env();
                            }
                        }
                    }
                    runtime.revision.fetch_add(1, Ordering::Release);
                    drop(runtime);
                    thread::sleep(Duration::from_secs(1).saturating_sub(started.elapsed()));
                }
            })
            .map_err(|e| e.to_string())?;
        *runtime.worker.lock().unwrap() = Some(handle);
        Ok(runtime)
    }
    pub fn shutdown(&self) {
        self.stop.store(true, Ordering::Release);
        if let Some(handle) = self.worker.lock().unwrap().take() {
            let _ = handle.join();
        }
    }
    pub fn revision(&self) -> u64 {
        self.revision.load(Ordering::Acquire)
    }
    pub fn status(&self) -> Status {
        self.status.lock().unwrap().clone()
    }
    pub fn take_alerts(&self) -> Vec<SpikeAlert> {
        std::mem::take(&mut *self.pending.lock().unwrap())
    }
    pub fn latest_alert(&self) -> Option<SpikeAlert> {
        self.latest.lock().unwrap().clone()
    }
    pub fn get_settings(&self) -> Result<Settings, String> {
        let settings = self.settings.lock().unwrap().clone();
        let available = resolve_data_dir(None)? == self.directory;
        Ok(Settings {
            alerts_enabled: settings.enabled,
            threshold_mib: settings.threshold_mib,
            startup_enabled: available && crate::startup::enabled()?,
            startup_available: available,
        })
    }
    pub fn save_settings(&self, settings: Settings) -> Result<(), String> {
        let alerts = AlertSettings {
            enabled: settings.alerts_enabled,
            threshold_mib: settings.threshold_mib,
        };
        alerts.validate()?;
        let current = self.get_settings()?;
        if !current.startup_available && settings.startup_enabled {
            return Err("独立测试目录不支持登录自启".into());
        }
        let (tx, rx) = mpsc::channel();
        self.control
            .send(Control::Settings(alerts, tx))
            .map_err(|e| e.to_string())?;
        rx.recv_timeout(Duration::from_secs(8))
            .map_err(|_| "设置保存超时，请检查采集状态".to_string())??;
        if current.startup_available && current.startup_enabled != settings.startup_enabled {
            crate::startup::set_enabled(
                settings.startup_enabled,
                &std::env::current_exe().map_err(|e| e.to_string())?,
                &self.directory,
            )?;
        }
        Ok(())
    }
    pub fn dashboard(&self, request: DashboardRequest) -> Result<DashboardSnapshot, String> {
        let day = parse_day(&request.day)?;
        validate_period(request.hour, request.minute)?;
        let now = Local::now();
        let today = now.date_naive();
        if day > today {
            return Err("请选择今天或更早日期".into());
        }
        let status = self.status();
        let store = self.store.lock().unwrap();
        let revision = self.revision();
        let tolerance = if status.connected { 3.5 } else { 0.0 };
        let index = match request.trend.as_str() {
            "hours" => 0,
            "7d" => 1,
            "30d" => 2,
            "week" => 3,
            "month" => 4,
            _ => return Err("未知趋势范围".into()),
        };
        let (start, end, days, hours, apps, selected) = if index > 0 {
            let start = trend_start(index, today).map_err(|e| e.to_string())?;
            let (rows, apps) = store
                .range_traffic(start, today)
                .map_err(|e| e.to_string())?;
            (
                start,
                today,
                rows.into_iter().map(day_row).collect::<Vec<_>>(),
                vec![],
                apps,
                None,
            )
        } else {
            let rows = store
                .day_hours(day, status.last_sample_at.map(|_| unix_now()), tolerance)
                .map_err(|e| e.to_string())?;
            let (selected, apps) = if let Some(minute) = request.minute {
                store
                    .minute_detail(day, request.hour, minute, Some(unix_now()), tolerance)
                    .map_err(|e| e.to_string())?
            } else {
                (
                    rows[request.hour as usize].clone(),
                    store
                        .hour_apps(day, request.hour)
                        .map_err(|e| e.to_string())?,
                )
            };
            let mut selected = hour_row(day, selected);
            if let Some(minute) = request.minute {
                selected.key = format!("{day}-{:02}-{minute:02}", request.hour);
                selected.label = format!(
                    "{:02}:{minute:02}–{:02}:{:02}",
                    request.hour,
                    request.hour + (minute + 5) / 60,
                    (minute + 5) % 60
                );
                selected.future =
                    day == today && (request.hour, minute) > (now.hour(), now.minute());
            }
            let selected = Some(selected);
            let rows = rows
                .into_iter()
                .map(|r| hour_row(day, r))
                .collect::<Vec<_>>();
            (day, day, vec![], rows, apps, selected)
        };
        let rows = if index > 0 { &days } else { &hours };
        let uploaded: i64 = rows
            .iter()
            .filter_map(|r| r.uploaded.parse::<i64>().ok())
            .sum();
        let downloaded: i64 = rows
            .iter()
            .filter_map(|r| r.downloaded.parse::<i64>().ok())
            .sum();
        let (unattributed_up, unattributed_down) = if index == 0 {
            let raw = store
                .day_hours(day, status.last_sample_at.map(|_| unix_now()), tolerance)
                .map_err(|e| e.to_string())?;
            (
                raw.iter().map(|r| r.unattributed_up).sum::<i64>(),
                raw.iter().map(|r| r.unattributed_down).sum::<i64>(),
            )
        } else {
            (0, 0)
        };
        let summary = Summary {
            uploaded: uploaded.to_string(),
            downloaded: downloaded.to_string(),
            covered_seconds: rows.iter().map(|r| r.covered_seconds).sum(),
            gap_seconds: rows.iter().map(|r| r.gap_seconds).sum(),
            legacy_coverage: rows.iter().any(|r| r.legacy_coverage),
            unattributed_up: unattributed_up.to_string(),
            unattributed_down: unattributed_down.to_string(),
        };
        Ok(DashboardSnapshot {
            revision,
            sampled_at: status.last_sample_at,
            status,
            days,
            hours,
            apps: apps.into_iter().map(Into::into).collect(),
            summary,
            selected,
            range_start: start.to_string(),
            range_end: end.to_string(),
            data_dir: self.directory.to_string_lossy().into(),
        })
    }
    pub fn targets(&self, request: TargetRequest) -> Result<Vec<RankRow>, String> {
        let day = parse_day(&request.day)?;
        if day > Local::now().date_naive() {
            return Err("请选择今天或更早日期".into());
        }
        if let Some(hour) = request.hour {
            validate_period(hour, request.minute)?
        } else if request.minute.is_some() {
            return Err("五分钟筛选必须指定小时".into());
        }
        self.store
            .lock()
            .unwrap()
            .app_targets_at(
                day,
                &request.app,
                request.hour,
                request.minute,
                Some(unix_now()),
            )
            .map(|rows| rows.into_iter().map(Into::into).collect())
            .map_err(|e| e.to_string())
    }
    pub fn diagnostics(&self) -> serde_json::Value {
        serde_json::json!({"effectiveDataDir":self.directory,"physicalDatabasePath":self.directory.join("traffic.sqlite3"),"revision":self.revision(),"status":self.status(),"defaultDataDir":default_data_dir().ok()})
    }
    fn apply_alert_settings(
        &self,
        settings: AlertSettings,
        detector: &mut SpikeDetector,
    ) -> Result<(), String> {
        let mut current = self.settings.lock().unwrap();
        if *current == settings {
            return Ok(());
        }
        self.store
            .lock()
            .unwrap()
            .save_alert_settings(&settings)
            .map_err(|e| e.to_string())?;
        detector.settings = settings.clone();
        detector.reset();
        *current = settings;
        self.pending.lock().unwrap().clear();
        Ok(())
    }
}
fn parse_day(value: &str) -> Result<NaiveDate, String> {
    let day = NaiveDate::parse_from_str(value, "%Y-%m-%d")
        .map_err(|_| "日期格式必须为 YYYY-MM-DD".to_string())?;
    if day.to_string() != value {
        return Err("日期格式必须为 YYYY-MM-DD".into());
    }
    Ok(day)
}
fn validate_period(hour: u32, minute: Option<u32>) -> Result<(), String> {
    if hour > 23 || minute.is_some_and(|m| m > 55 || m % 5 != 0) {
        return Err("小时或五分钟区间无效".into());
    }
    Ok(())
}
fn hour_row(day: NaiveDate, r: HourTraffic) -> TrafficRow {
    let now = Local::now();
    TrafficRow {
        key: r.hour.to_string(),
        label: format!("{:02}:00", r.hour),
        day: day.to_string(),
        hour: Some(r.hour),
        uploaded: r.uploaded.to_string(),
        downloaded: r.downloaded.to_string(),
        samples: r.samples,
        covered_seconds: r.covered_seconds,
        gap_seconds: r.gap_seconds,
        legacy_coverage: r.legacy_coverage,
        future: day == now.date_naive() && r.hour > now.hour(),
    }
}
fn day_row(r: DayTraffic) -> TrafficRow {
    TrafficRow {
        key: r.day.to_string(),
        label: r.day.format("%m-%d").to_string(),
        day: r.day.to_string(),
        hour: None,
        uploaded: r.uploaded.to_string(),
        downloaded: r.downloaded.to_string(),
        samples: r.samples,
        covered_seconds: r.covered_seconds,
        gap_seconds: r.gap_seconds,
        legacy_coverage: r.legacy_coverage,
        future: false,
    }
}
pub fn alert_body(alert: &SpikeAlert) -> String {
    let start = monitor_core::time::local_at(alert.start);
    let end = monitor_core::time::local_at(alert.end);
    let names = alert
        .contributors
        .iter()
        .map(|name| {
            if name.is_empty() {
                "未知应用"
            } else {
                name.as_str()
            }
        })
        .collect::<Vec<_>>()
        .join("、");
    format!(
        "{}–{} 代理流量 {:.1} MiB。主要应用：{}。点击查看高峰时段。",
        start.format("%m-%d %H:%M"),
        end.format("%H:%M"),
        alert.total as f64 / 1_048_576.0,
        names
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reject_bad_filters() {
        assert!(parse_day("2026-02-31").is_err());
        assert!(parse_day("2026-1-01").is_err());
        assert!(validate_period(24, None).is_err());
        assert!(validate_period(1, Some(4)).is_err());
        assert!(validate_period(1, Some(55)).is_ok());
    }
    #[test]
    fn bytes_serialize_without_js_precision_loss() {
        let row = RankRow::from(AppTraffic {
            app: None,
            uploaded: 9007199254740993,
            downloaded: 0,
        });
        let json = serde_json::to_value(row).unwrap();
        assert_eq!(json["uploaded"], "9007199254740993");
    }
    #[test]
    fn original_settings_preserve_window_and_hour_keys_match_frontend() {
        let temp = tempfile::tempdir().unwrap();
        let settings = AlertSettings {
            enabled: true,
            threshold_mib: 1,
        };
        let store = Store::open(temp.path().join("traffic.sqlite3")).unwrap();
        store.save_alert_settings(&settings).unwrap();
        drop(store);
        let runtime = Runtime::start_with_source(
            temp.path().to_path_buf(),
            Ok(MihomoSource::with_transports(vec![])),
        )
        .unwrap();
        let mut detector = SpikeDetector::new(settings.clone());
        let apps = monitor_core::AppDeltas::from([("app.exe".into(), (1_048_576, 0))]);
        assert!(detector.add(0.0, 1.0, &apps).is_none());
        runtime
            .apply_alert_settings(settings, &mut detector)
            .unwrap();
        assert_eq!(detector.total, 1_048_576);
        assert!(detector.add(899.0, 900.0, &Default::default()).is_some());
        runtime
            .apply_alert_settings(
                AlertSettings {
                    enabled: true,
                    threshold_mib: 2,
                },
                &mut detector,
            )
            .unwrap();
        assert_eq!(detector.total, 0);
        let row = hour_row(
            Local::now().date_naive(),
            HourTraffic {
                hour: 13,
                ..Default::default()
            },
        );
        assert_eq!(row.key, "13");
        let text = alert_body(&SpikeAlert {
            start: 0.0,
            end: 900.0,
            total: 1_048_576,
            contributors: vec!["".into(), "app.exe".into()],
            peak_at: 0.0,
        });
        assert!(text.contains("1.0 MiB") && text.contains("未知应用、app.exe"));
        runtime.shutdown();
    }
}
