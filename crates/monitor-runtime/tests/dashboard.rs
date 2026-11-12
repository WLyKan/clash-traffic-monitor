//! Public runtime/DTO integration with frozen, isolated SQLite samples.
//! An empty successful source never falls back to the user's live controller.
use chrono::{Duration, Local, NaiveDate, TimeZone, Timelike};
use monitor_core::{AlertSettings, MihomoSource, SampleRecord};
use monitor_runtime::{
    backend::{DashboardRequest, RankRow, Runtime, TargetRequest},
    cli,
    paths::{resolve_data_dir, WriterLock},
};
use monitor_store::Store;
use rusqlite::{params, Connection};
use serde_json::Value;
use std::{collections::BTreeMap, path::Path, sync::Arc};
use tempfile::TempDir;

const LARGE: i64 = 9_007_199_254_740_993;

struct Harness {
    _directory: TempDir,
    runtime: Option<Arc<Runtime>>,
    day: NaiveDate,
}
impl Harness {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let physical = resolve_data_dir(Some(directory.path())).unwrap();
        let day = Local::now().date_naive() - Duration::days(1);
        {
            let mut store = Store::open(physical.join("traffic.sqlite3")).unwrap();
            store
                .save_alert_settings(&AlertSettings {
                    enabled: true,
                    threshold_mib: 147,
                })
                .unwrap();
            for (hour, minute, up, down, target) in [
                (10, 1, LARGE, 11, Some("example.com")),
                (10, 6, 7, 13, Some("目标未知")),
                (11, 1, 17, 19, None),
            ] {
                let sampled_at = Local
                    .from_local_datetime(&day.and_hms_opt(hour, minute, 1).unwrap())
                    .single()
                    .unwrap()
                    .timestamp() as f64;
                let mut record = SampleRecord {
                    sampled_at,
                    covered_from: Some(sampled_at - 1.0),
                    segment: Some(sampled_at - 1.0),
                    ..Default::default()
                };
                record.apps.insert("Code.exe".into(), (up, down));
                if let Some(target) = target {
                    record
                        .targets
                        .insert(("Code.exe".into(), target.into()), (up, down));
                }
                store.record(&record).unwrap();
            }
            // Represents existing imported history for which exact collection
            // coverage was never recorded. Preserve its quality marker in DTOs.
            Connection::open(store.path())
                .unwrap()
                .execute(
                    "UPDATE hours SET legacy_coverage=1 WHERE day=?1 AND hour=11",
                    [day.to_string()],
                )
                .unwrap();
        }
        let runtime =
            Runtime::start_with_source(physical, Ok(MihomoSource::with_transports(vec![])))
                .unwrap();
        Self {
            _directory: directory,
            runtime: Some(runtime),
            day,
        }
    }
    fn runtime(&self) -> &Runtime {
        self.runtime.as_ref().unwrap()
    }
    fn request(&self, hour: u32, minute: Option<u32>) -> DashboardRequest {
        DashboardRequest {
            day: self.day.to_string(),
            trend: "hours".into(),
            hour,
            minute,
        }
    }
    fn targets(&self, hour: Option<u32>, minute: Option<u32>) -> Vec<RankRow> {
        self.runtime()
            .targets(TargetRequest {
                day: self.day.to_string(),
                app: "code.EXE".into(),
                hour,
                minute,
            })
            .unwrap()
    }
}
impl Drop for Harness {
    fn drop(&mut self) {
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown();
            let path = runtime.directory.clone();
            drop(runtime);
            assert!(
                WriterLock::acquire(&path).is_ok(),
                "runtime must release its writer lock"
            );
        }
    }
}

fn totals(rows: &[RankRow]) -> (i64, i64) {
    rows.iter().fold((0, 0), |(up, down), row| {
        (
            up + row.uploaded.parse::<i64>().unwrap(),
            down + row.downloaded.parse::<i64>().unwrap(),
        )
    })
}

#[test]
fn dashboard_and_targets_preserve_day_hour_minute_totals_and_large_byte_strings() {
    let harness = Harness::new();
    let runtime = harness.runtime();
    let hour = runtime.dashboard(harness.request(10, None)).unwrap();
    assert_eq!(hour.hours.len(), 24);
    assert_eq!(
        (
            hour.summary.uploaded.as_str(),
            hour.summary.downloaded.as_str()
        ),
        ((LARGE + 24).to_string().as_str(), "43")
    );
    assert_eq!(
        (
            hour.selected.as_ref().unwrap().uploaded.as_str(),
            hour.selected.as_ref().unwrap().downloaded.as_str()
        ),
        ((LARGE + 7).to_string().as_str(), "24")
    );
    assert_eq!(totals(&hour.apps), (LARGE + 7, 24));
    assert_eq!(totals(&harness.targets(Some(10), None)), totals(&hour.apps));
    let minute = runtime.dashboard(harness.request(10, Some(0))).unwrap();
    let selected = minute.selected.as_ref().unwrap();
    assert_eq!(
        (selected.uploaded.as_str(), selected.downloaded.as_str()),
        ("9007199254740993", "11")
    );
    assert_eq!(selected.label, "10:00–10:05");
    assert_eq!((selected.samples, selected.covered_seconds), (1, 1.0));
    assert_eq!(totals(&minute.apps), (LARGE, 11));
    let target = harness.targets(Some(10), Some(0));
    assert_eq!(target[0].name, "example.com");
    assert_eq!(target[0].uploaded, "9007199254740993");
    assert_eq!(totals(&target), totals(&minute.apps));
    let day_targets = harness.targets(None, None);
    assert_eq!(totals(&day_targets), (LARGE + 24, 43));
    assert_eq!(
        day_targets
            .iter()
            .map(|row| row.name.as_str())
            .collect::<Vec<_>>(),
        vec!["example.com", "历史未记录域名", "目标未知"]
    );
    let legacy = runtime.dashboard(harness.request(11, None)).unwrap();
    assert!(legacy.selected.unwrap().legacy_coverage);
    assert!(legacy.summary.legacy_coverage);
    let json = serde_json::to_value(minute).unwrap();
    assert!(json["summary"]["uploaded"].is_string());
    assert!(json["selected"]["uploaded"].is_string());
    assert!(json["apps"][0]["uploaded"].is_string());
    assert_eq!(json["hours"][10]["uploaded"], (LARGE + 7).to_string());
    let settings = runtime.get_settings().unwrap();
    assert!(settings.alerts_enabled);
    assert_eq!(settings.threshold_mib, 147);
    assert!(!settings.startup_available);
    assert!(!settings.startup_enabled);
    assert_eq!(Path::new(&hour.data_dir), runtime.directory);
}

#[test]
fn daily_dto_omits_optional_hour_instead_of_serializing_null() {
    let harness = Harness::new();
    let mut request = harness.request(10, None);
    request.trend = "7d".into();
    let result = harness.runtime().dashboard(request).unwrap();
    assert_eq!(result.days.len(), 7);
    assert!(result.hours.is_empty());
    assert!(result.selected.is_none());
    let row = result
        .days
        .iter()
        .find(|row| row.day == harness.day.to_string())
        .unwrap();
    assert_eq!(row.uploaded, (LARGE + 24).to_string());
    assert!(row.legacy_coverage);
    let json = serde_json::to_value(&result).unwrap();
    for row in json["days"].as_array().unwrap() {
        assert!(
            row.get("hour").is_none(),
            "Zod optional number rejects hour:null: {row}"
        );
        assert!(row["uploaded"].is_string());
        assert!(row["legacyCoverage"].is_boolean());
    }
    assert_eq!(json["selected"], Value::Null);
    assert_eq!(totals(&result.apps), (LARGE + 24, 43));
    assert!(result.summary.legacy_coverage);
}

#[test]
fn runtime_rejects_invalid_dates_periods_trends_and_future_dates() {
    let harness = Harness::new();
    for day in [
        "2026-02-31",
        "2026-1-01",
        "not-a-date",
        "2026-01-01T00:00:00",
    ] {
        let mut request = harness.request(10, None);
        request.day = day.into();
        assert!(harness.runtime().dashboard(request).is_err());
        assert!(harness
            .runtime()
            .targets(TargetRequest {
                day: day.into(),
                app: "Code.exe".into(),
                hour: None,
                minute: None
            })
            .is_err());
    }
    for (hour, minute) in [(24, None), (10, Some(1)), (10, Some(60))] {
        assert!(harness
            .runtime()
            .dashboard(harness.request(hour, minute))
            .is_err());
        assert!(harness
            .runtime()
            .targets(TargetRequest {
                day: harness.day.to_string(),
                app: "Code.exe".into(),
                hour: Some(hour),
                minute
            })
            .is_err());
    }
    assert!(harness
        .runtime()
        .targets(TargetRequest {
            day: harness.day.to_string(),
            app: "Code.exe".into(),
            hour: None,
            minute: Some(0)
        })
        .is_err());
    let mut request = harness.request(10, None);
    request.trend = "unsupported".into();
    assert!(harness.runtime().dashboard(request).is_err());
    let tomorrow = (Local::now().date_naive() + Duration::days(1)).to_string();
    let mut request = harness.request(10, None);
    request.day = tomorrow.clone();
    assert!(harness.runtime().dashboard(request).is_err());
    assert!(harness
        .runtime()
        .targets(TargetRequest {
            day: tomorrow,
            app: "Code.exe".into(),
            hour: None,
            minute: None
        })
        .is_err());
}

#[test]
fn today_future_hour_and_five_minute_flags_follow_the_whole_time_interval() {
    let harness = Harness::new();
    // Re-read if the test happened to cross a minute boundary while querying.
    for _ in 0..3 {
        let before = Local::now();
        let mut request = harness.request(before.hour(), None);
        request.day = before.date_naive().to_string();
        let result = harness.runtime().dashboard(request.clone()).unwrap();
        for row in &result.hours {
            assert_eq!(row.future, row.hour.unwrap() > before.hour());
        }
        let current_minute = before.minute() / 5 * 5;
        let mut cases = vec![(before.hour(), current_minute, false)];
        if current_minute < 55 {
            cases.push((before.hour(), current_minute + 5, true));
        }
        if before.hour() < 23 {
            cases.push((before.hour() + 1, 0, true));
        }
        let mut checks = Vec::new();
        for (hour, minute, expected) in cases {
            request.hour = hour;
            request.minute = Some(minute);
            let snapshot = harness.runtime().dashboard(request.clone()).unwrap();
            let selected = snapshot.selected.unwrap();
            checks.push((selected.future, expected));
            assert_eq!(
                (
                    selected.uploaded.as_str(),
                    selected.downloaded.as_str(),
                    selected.samples
                ),
                ("0", "0", 0)
            );
        }
        let after = Local::now();
        if (before.date_naive(), before.hour(), before.minute())
            == (after.date_naive(), after.hour(), after.minute())
        {
            for (actual, expected) in checks {
                assert_eq!(actual, expected);
            }
            return;
        }
    }
    panic!("clock changed across all three future-flag attempts");
}

fn directory_contents(directory: &Path) -> BTreeMap<String, Vec<u8>> {
    std::fs::read_dir(directory)
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            (
                entry.file_name().to_string_lossy().into_owned(),
                std::fs::read(entry.path()).unwrap(),
            )
        })
        .collect()
}

#[test]
fn readonly_diagnostics_preserve_legacy_database_files_and_missing_directory() {
    let temp = tempfile::tempdir().unwrap();
    let directory = temp.path().join("数据 # %");
    std::fs::create_dir(&directory).unwrap();
    let database = directory.join("traffic.sqlite3");
    {
        let db = Connection::open(&database).unwrap();
        db.execute_batch(
            "CREATE TABLE hours(day TEXT,hour INTEGER,samples INTEGER,gap_seconds REAL);
            CREATE TABLE apps(day TEXT,hour INTEGER,app TEXT,uploaded INTEGER,downloaded INTEGER);
            CREATE TABLE checkpoint(id INTEGER,sampled_at REAL);
            CREATE TABLE history_imports(source_key TEXT PRIMARY KEY,digest TEXT,imported_at REAL);
            INSERT INTO hours VALUES('2026-01-01',10,3,2.5);
            INSERT INTO checkpoint VALUES(1,12345.5);
            INSERT INTO history_imports VALUES('original','preserved',1.0);",
        )
        .unwrap();
        db.execute(
            "INSERT INTO apps VALUES('2026-01-01',10,'Code.exe',?1,11)",
            params![LARGE],
        )
        .unwrap();
    }
    let before = directory_contents(&directory);
    let report = cli::diagnostics(&directory).unwrap();
    assert!(report.get("database_error").is_none(), "{report}");
    assert_eq!(report["days"][0]["uploaded"], LARGE);
    assert_eq!(report["days"][0]["covered_seconds"], Value::Null);
    assert_eq!(report["checkpoint"]["sampled_at"], 12345.5);
    assert_eq!(report["snapshot_mode"], "immutable");
    assert_eq!(
        report["physical_database_path"],
        serde_json::to_value(
            resolve_data_dir(Some(&directory))
                .unwrap()
                .join("traffic.sqlite3")
        )
        .unwrap()
    );
    assert_eq!(
        directory_contents(&directory),
        before,
        "diagnostics must not migrate, create locks, or write WAL/SHM"
    );
    let missing = temp.path().join("missing").join("nested");
    let report = cli::diagnostics(&missing).unwrap();
    assert_eq!(report["database_exists"], false);
    assert!(!missing.exists());
}
