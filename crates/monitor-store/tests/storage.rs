use chrono::{Local, NaiveDate, NaiveDateTime, TimeZone};
use monitor_store::{
    AlertSettings, AppTraffic, DayTraffic, HourTraffic, SampleRecord, Store, DETAIL_RETENTION,
};
use rusqlite::Connection;
use serde_json::{json, Value};
use tempfile::TempDir;

fn date(value: &str) -> NaiveDate {
    NaiveDate::parse_from_str(value, "%Y-%m-%d").unwrap()
}
fn stamp(value: &str) -> f64 {
    Local
        .from_local_datetime(&NaiveDateTime::parse_from_str(value, "%Y-%m-%dT%H:%M:%S").unwrap())
        .single()
        .unwrap()
        .timestamp() as f64
}
fn sample(local: &str) -> SampleRecord {
    SampleRecord {
        sampled_at: stamp(local),
        ..Default::default()
    }
}
fn fixture() -> Value {
    serde_json::from_str(include_str!(
        "../../../tests/fixtures/rust-store/python-contract.json"
    ))
    .unwrap()
}

#[test]
fn python_query_contract_matches_rust_in_local_time() {
    let directory = TempDir::new().unwrap();
    let mut store = Store::open(directory.path().join("traffic.sqlite3")).unwrap();
    let fixture = fixture();
    for input in fixture["records"].as_array().unwrap() {
        let mut record = sample(input["local"].as_str().unwrap());
        record.unmatched = (
            input["unmatched"][0].as_i64().unwrap(),
            input["unmatched"][1].as_i64().unwrap(),
        );
        record.covered_from = input["covered_seconds"]
            .as_f64()
            .map(|v| record.sampled_at - v);
        record.gap_from = input["gap_seconds"].as_f64().map(|v| record.sampled_at - v);
        record.segment = input["segment"].as_str().map(stamp);
        for row in input["apps"].as_array().unwrap() {
            record.apps.insert(
                row[0].as_str().unwrap().to_string(),
                (row[1].as_i64().unwrap(), row[2].as_i64().unwrap()),
            );
        }
        for row in input["targets"].as_array().unwrap() {
            record.targets.insert(
                (
                    row[0].as_str().unwrap().to_string(),
                    row[1].as_str().unwrap().to_string(),
                ),
                (row[2].as_i64().unwrap(), row[3].as_i64().unwrap()),
            );
        }
        store.record(&record).unwrap();
    }
    let expected = &fixture["expected"];
    for (day, rows) in expected["day_hours"].as_object().unwrap() {
        assert_eq!(
            store.day_hours(date(day), None, 3.5).unwrap(),
            serde_json::from_value::<Vec<HourTraffic>>(rows.clone()).unwrap(),
            "day {day}"
        );
    }
    let (days, apps) = store
        .range_traffic(date("2026-11-11"), date("2026-11-12"))
        .unwrap();
    assert_eq!(
        days,
        serde_json::from_value::<Vec<DayTraffic>>(expected["range_days"].clone()).unwrap()
    );
    assert_eq!(serde_json::to_value(apps).unwrap(), expected["range_apps"]);
    for query in expected["minutes"].as_array().unwrap() {
        let (traffic, apps) = store
            .minute_detail(
                date(query["day"].as_str().unwrap()),
                query["hour"].as_u64().unwrap() as u32,
                query["minute"].as_u64().unwrap() as u32,
                None,
                3.5,
            )
            .unwrap();
        assert_eq!(
            traffic,
            serde_json::from_value::<HourTraffic>(query["traffic"].clone()).unwrap(),
            "minute {query}"
        );
        assert_eq!(serde_json::to_value(apps).unwrap(), query["apps"]);
    }
    for query in expected["targets"].as_array().unwrap() {
        let rows = store
            .app_targets(
                date(query["day"].as_str().unwrap()),
                "Code.exe",
                query["hour"].as_u64().map(|n| n as u32),
                query["minute"].as_u64().map(|n| n as u32),
            )
            .unwrap();
        assert_eq!(serde_json::to_value(rows).unwrap(), query["rows"]);
    }
    store.check_integrity().unwrap();
}

#[test]
fn original_schema_upgrade_preserves_history_settings_ledger_and_checkpoint() {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("legacy.sqlite3");
    {
        let db = Connection::open(&path).unwrap();
        db.execute_batch(include_str!("../src/schema.sql")).unwrap();
        db.execute_batch("INSERT INTO hours(day,hour,samples,gap_seconds) VALUES('2026-11-11',7,3,5.5);
            INSERT INTO apps VALUES('2026-11-11',7,'Code.exe',11,22);
            INSERT INTO checkpoint VALUES(1,1234567.5);
            INSERT INTO spike_settings VALUES(1,1,777);
            CREATE TABLE history_imports(source_key TEXT PRIMARY KEY,digest TEXT NOT NULL,imported_at REAL NOT NULL);
            INSERT INTO history_imports VALUES('verified-original','digest',123.0);").unwrap();
    }
    let store = Store::open(&path).unwrap();
    let hour = &store.day_hours(date("2026-11-11"), None, 3.5).unwrap()[7];
    assert_eq!(
        (
            hour.uploaded,
            hour.downloaded,
            hour.samples,
            hour.gap_seconds
        ),
        (11, 22, 3, 5.5)
    );
    assert!(hour.legacy_coverage);
    assert_eq!(hour.covered_seconds, 0.0);
    assert_eq!(store.last_sample().unwrap(), Some(1234567.5));
    assert_eq!(
        store.load_alert_settings().unwrap(),
        Some(AlertSettings {
            enabled: true,
            threshold_mib: 777
        })
    );
    assert_eq!(
        store
            .app_targets(date("2026-11-11"), "code.EXE", Some(7), None)
            .unwrap(),
        vec![AppTraffic {
            app: Some("历史未记录域名".into()),
            uploaded: 11,
            downloaded: 22
        }]
    );
    let db = Connection::open(&path).unwrap();
    assert_eq!(
        db.query_row("SELECT digest FROM history_imports", [], |r| r
            .get::<_, String>(0))
            .unwrap(),
        "digest"
    );
    drop(db);
    drop(store);
    let again = Store::open(&path).unwrap();
    assert_eq!(
        again.day_hours(date("2026-11-11"), None, 3.5).unwrap()[7].uploaded,
        11
    );
}

#[test]
fn target_failure_rolls_back_all_dimensions_and_checkpoint() {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("traffic.sqlite3");
    let mut store = Store::open(&path).unwrap();
    let mut first = sample("2026-11-12T10:04:59");
    first.apps.insert("Code.exe".into(), (10, 20));
    store.record(&first).unwrap();
    Connection::open(&path).unwrap().execute_batch("CREATE TRIGGER abort_target BEFORE INSERT ON targets BEGIN SELECT RAISE(ABORT,'test write failure');END;").unwrap();
    let mut failing = sample("2026-11-12T10:05:01");
    failing.apps.insert("Code.exe".into(), (100, 200));
    failing
        .targets
        .insert(("Code.exe".into(), "example.com".into()), (100, 200));
    failing.covered_from = Some(first.sampled_at);
    failing.segment = Some(first.sampled_at);
    assert!(store.record(&failing).is_err());
    assert_eq!(store.last_sample().unwrap(), Some(first.sampled_at));
    let hour = &store.day_hours(date("2026-11-12"), None, 3.5).unwrap()[10];
    assert_eq!(
        (
            hour.uploaded,
            hour.downloaded,
            hour.samples,
            hour.covered_seconds
        ),
        (10, 20, 1, 0.0)
    );
    assert_eq!(
        store
            .minute_detail(date("2026-11-12"), 10, 5, None, 3.5)
            .unwrap()
            .0
            .samples,
        0
    );
}

#[test]
fn signed_residual_moves_only_offsetting_bytes_across_midnight_within_segment() {
    let directory = TempDir::new().unwrap();
    let mut store = Store::open(directory.path().join("traffic.sqlite3")).unwrap();
    let mut first = sample("2026-11-11T23:59:59");
    first.covered_from = Some(first.sampled_at - 2.0);
    first.segment = first.covered_from;
    first.unmatched = (10, -9);
    store.record(&first).unwrap();
    let mut second = sample("2026-11-12T00:00:01");
    second.covered_from = Some(first.sampled_at);
    second.segment = first.segment;
    second.unmatched = (-7, 5);
    store.record(&second).unwrap();
    let previous = store
        .day_hours(date("2026-11-11"), None, 3.5)
        .unwrap()
        .remove(23);
    let next = store
        .day_hours(date("2026-11-12"), None, 3.5)
        .unwrap()
        .remove(0);
    assert_eq!(
        (
            previous.unattributed_up,
            previous.unattributed_down,
            next.unattributed_up,
            next.unattributed_down
        ),
        (3, 0, 0, 0)
    );
    assert_eq!((previous.covered_seconds, next.covered_seconds), (3.0, 1.0));
    let mut third = sample("2026-11-12T00:05:01");
    third.covered_from = Some(third.sampled_at - 2.0);
    third.segment = third.covered_from;
    third.unmatched = (20, 10);
    store.record(&third).unwrap();
    let mut fourth = sample("2026-11-12T00:10:01");
    fourth.covered_from = Some(fourth.sampled_at - 2.0);
    fourth.segment = fourth.covered_from;
    fourth.unmatched = (-20, -10);
    store.record(&fourth).unwrap();
    assert_eq!(
        store
            .minute_detail(date("2026-11-12"), 0, 5, None, 3.5)
            .unwrap()
            .0
            .unattributed_up,
        20
    );
    assert_eq!(
        store
            .minute_detail(date("2026-11-12"), 0, 10, None, 3.5)
            .unwrap()
            .0
            .unattributed_up,
        0
    );
}

#[test]
fn retention_cascades_all_minute_dimensions_but_keeps_hour_history() {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("traffic.sqlite3");
    let mut store = Store::open(&path).unwrap();
    let mut old = sample("2026-10-01T10:04:59");
    old.apps.insert("Code.exe".into(), (10, 20));
    old.targets
        .insert(("Code.exe".into(), "example.com".into()), (10, 20));
    old.covered_from = Some(old.sampled_at - 2.0);
    old.segment = old.covered_from;
    old.unmatched = (1, 2);
    store.record(&old).unwrap();
    let recent = SampleRecord {
        sampled_at: old.sampled_at + DETAIL_RETENTION + 300.0,
        ..Default::default()
    };
    store.record(&recent).unwrap();
    let db = Connection::open(&path).unwrap();
    for table in ["minute_apps", "minute_targets", "minute_residuals"] {
        assert_eq!(
            db.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0,
            "{table}"
        );
    }
    assert_eq!(
        store.hour_apps(date("2026-10-01"), 10).unwrap()[0].uploaded,
        10
    );
    assert_eq!(
        store
            .app_targets(date("2026-10-01"), "Code.exe", None, None)
            .unwrap()[0]
            .downloaded,
        20
    );
    assert_eq!(
        store
            .minute_detail(date("2026-10-01"), 10, 0, Some(recent.sampled_at), 3.5)
            .unwrap()
            .0
            .samples,
        0
    );
    store.check_integrity().unwrap();
}

#[test]
fn old_checkpoint_does_not_invent_five_minute_coverage_and_stationary_gaps_update() {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("traffic.sqlite3");
    let mut store = Store::open(&path).unwrap();
    let now = stamp("2026-11-12T10:05:01");
    let previous = now - 300.0;
    Connection::open(&path)
        .unwrap()
        .execute("INSERT INTO checkpoint VALUES(1,?1)", [previous])
        .unwrap();
    let record = SampleRecord {
        sampled_at: now,
        gap_from: Some(previous),
        ..Default::default()
    };
    store.record(&record).unwrap();
    assert_eq!(
        store.day_hours(date("2026-11-12"), None, 3.5).unwrap()[10].gap_seconds,
        300.0
    );
    assert_eq!(
        store
            .minute_detail(date("2026-11-12"), 10, 0, None, 3.5)
            .unwrap()
            .0
            .gap_seconds,
        0.0
    );
    assert_eq!(
        store
            .minute_detail(date("2026-11-12"), 10, 5, Some(now + 10.0), 3.5)
            .unwrap()
            .0
            .gap_seconds,
        10.0
    );
    assert_eq!(
        store
            .day_hours(date("2026-11-12"), Some(now + 10.0), 3.5)
            .unwrap()[10]
            .gap_seconds,
        310.0
    );
    assert_eq!(
        store
            .minute_detail(date("2026-11-12"), 10, 5, Some(now + 20.0), 3.5)
            .unwrap()
            .0
            .gap_seconds,
        20.0
    );
}

#[test]
fn backup_carries_committed_wal_and_is_non_destructive() {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("live.sqlite3");
    let mut store = Store::open(&path).unwrap();
    let record = sample("2026-11-12T10:00:00");
    store.record(&record).unwrap();
    store
        .save_alert_settings(&AlertSettings {
            enabled: true,
            threshold_mib: 321,
        })
        .unwrap();
    let backup = directory.path().join("snapshot.sqlite3");
    store.backup_to(&backup).unwrap();
    let restored = Store::open_read_only(&backup).unwrap();
    assert_eq!(restored.last_sample().unwrap(), Some(record.sampled_at));
    assert_eq!(
        restored.load_alert_settings().unwrap(),
        Some(AlertSettings {
            enabled: true,
            threshold_mib: 321
        })
    );
    assert!(store.backup_to(&backup).is_err());
    assert!(store.backup_to(&path).is_err());
    restored.check_integrity().unwrap();
}

#[test]
fn validation_rejects_invalid_intervals_and_preserves_zero_sample_semantics() {
    let directory = TempDir::new().unwrap();
    let mut store = Store::open(directory.path().join("traffic.sqlite3")).unwrap();
    assert_eq!(
        store.day_hours(date("2026-11-12"), None, 3.5).unwrap()[3].samples,
        0
    );
    let zero = sample("2026-11-12T03:00:00");
    store.record(&zero).unwrap();
    let hour = &store.day_hours(date("2026-11-12"), None, 3.5).unwrap()[3];
    assert_eq!((hour.samples, hour.uploaded, hour.downloaded), (1, 0, 0));
    let invalid = SampleRecord {
        covered_from: Some(zero.sampled_at + 1.0),
        ..zero.clone()
    };
    assert!(store.record(&invalid).is_err());
    assert!(store
        .minute_detail(date("2026-11-12"), 24, 0, None, 3.5)
        .is_err());
    assert!(store
        .minute_detail(date("2026-11-12"), 3, 1, None, 3.5)
        .is_err());
    assert!(store
        .app_targets(date("2026-11-12"), "app", None, Some(0))
        .is_err());
    assert_eq!(json!(store.last_sample().unwrap()), json!(zero.sampled_at));
}

#[test]
fn byte_counts_above_javascript_integer_limit_remain_exact() {
    let directory = TempDir::new().unwrap();
    let mut store = Store::open(directory.path().join("traffic.sqlite3")).unwrap();
    let value = 9_007_199_254_740_993i64;
    let mut record = sample("2026-11-12T10:00:01");
    record.apps.insert("Code.exe".into(), (value, value + 2));
    record.targets.insert(
        ("Code.exe".into(), "example.com".into()),
        (value, value + 2),
    );
    store.record(&record).unwrap();
    let hour = &store.day_hours(date("2026-11-12"), None, 3.5).unwrap()[10];
    assert_eq!((hour.uploaded, hour.downloaded), (value, value + 2));
    let target = &store
        .app_targets(date("2026-11-12"), "Code.exe", Some(10), Some(0))
        .unwrap()[0];
    assert_eq!((target.uploaded, target.downloaded), (value, value + 2));
    let (days, apps) = store
        .range_traffic(date("2026-11-12"), date("2026-11-12"))
        .unwrap();
    assert_eq!((days[0].uploaded, apps[0].downloaded), (value, value + 2));
}

#[test]
fn clock_rollback_rebaselines_through_store_then_resumes_continuous_sampling() {
    use monitor_core::{SamplingState, Snapshot};
    let directory = TempDir::new().unwrap();
    let mut store = Store::open(directory.path().join("traffic.sqlite3")).unwrap();
    let mut state = SamplingState::new(None, 1.0);
    for now in [100.0, 99.0, 100.0] {
        let snapshot = Snapshot {
            sampled_at: now,
            ..Default::default()
        };
        let record = state.prepare(&snapshot);
        if state.last_sample == Some(100.0) && now == 99.0 {
            assert_eq!(record.gap_from, None);
            assert_eq!(record.covered_from, None);
        }
        if state.last_sample == Some(99.0) {
            assert_eq!(record.covered_from, Some(99.0));
        }
        store.record(&record).unwrap();
        assert_eq!(store.last_sample().unwrap(), Some(now));
        state.commit(snapshot, &record);
    }
    let day = monitor_store::local_timestamp(100.0).unwrap().date_naive();
    let hours = store.day_hours(day, None, 3.5).unwrap();
    assert_eq!(hours.iter().map(|row| row.samples).sum::<i64>(), 3);
    assert_eq!(
        hours.iter().map(|row| row.covered_seconds).sum::<f64>(),
        1.0
    );
    assert_eq!(hours.iter().map(|row| row.gap_seconds).sum::<f64>(), 0.0);
}

#[test]
fn offline_query_time_retention_expires_minute_targets_with_their_app_details() {
    let directory = TempDir::new().unwrap();
    let mut store = Store::open(directory.path().join("traffic.sqlite3")).unwrap();
    let mut record = sample("2026-11-12T10:00:01");
    record.apps.insert("Code.exe".into(), (10, 20));
    record
        .targets
        .insert(("Code.exe".into(), "example.com".into()), (10, 20));
    store.record(&record).unwrap();
    let now = record.sampled_at + DETAIL_RETENTION + 300.0;
    let day = date("2026-11-12");
    assert_eq!(
        store
            .minute_detail(day, 10, 0, Some(now), 3.5)
            .unwrap()
            .0
            .samples,
        0
    );
    assert!(store
        .app_targets_at(day, "Code.exe", Some(10), Some(0), Some(now))
        .unwrap()
        .is_empty());
    // No newer record was written, so the physical detail still exists; the
    // live API must filter it rather than depending on a pruning write.
    assert_eq!(
        store
            .app_targets(day, "Code.exe", Some(10), Some(0))
            .unwrap()[0]
            .uploaded,
        10
    );
    for hour in [None, Some(10)] {
        let rows = store
            .app_targets_at(day, "Code.exe", hour, None, Some(now))
            .unwrap();
        assert_eq!((rows[0].uploaded, rows[0].downloaded), (10, 20));
    }
}
