use chrono::NaiveDate;
use monitor_store::{
    recovery::{backup_database, merge_history, HistorySource},
    Store,
};
use rusqlite::{params, Connection};
use serde_json::Value;
use std::path::Path;
use tempfile::TempDir;

fn fixture() -> Value {
    serde_json::from_str(include_str!(
        "../../../tests/fixtures/rust-store/python-contract.json"
    ))
    .unwrap()
}
fn make_source(directory: &Path, name: &str) -> HistorySource {
    let path = directory.join(format!("{name}-snapshot.sqlite3"));
    let db = Connection::open(&path).unwrap();
    // Python iterdump emits child tables before minutes, and its restore
    // contract requires temporarily disabling FK checks during that SQL dump.
    db.execute_batch("PRAGMA foreign_keys=OFF;").unwrap();
    db.execute_batch(fixture()["snapshot_sql"].as_str().unwrap())
        .unwrap();
    db.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
    HistorySource {
        snapshot: path,
        source_key: directory
            .join(format!("{name}-original.sqlite3"))
            .to_string_lossy()
            .into_owned(),
    }
}
fn total(path: &Path) -> (i64, i64) {
    Connection::open(path)
        .unwrap()
        .query_row("SELECT SUM(uploaded),SUM(downloaded) FROM apps", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap()
}
fn day() -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 11, 12).unwrap()
}
fn key(source: &HistorySource) -> String {
    canonical_key(&source.source_key)
}
fn canonical_key(value: &str) -> String {
    #[cfg(windows)]
    {
        value.replace('/', "\\").to_lowercase()
    }
    #[cfg(not(windows))]
    {
        value.to_string()
    }
}

#[test]
fn full_recovery_includes_targets_settings_checkpoint_and_is_idempotent() {
    let directory = TempDir::new().unwrap();
    let target = directory.path().join("target.sqlite3");
    let source = make_source(directory.path(), "source");
    let original = total(&source.snapshot);
    let result = merge_history(&target, std::slice::from_ref(&source)).unwrap();
    assert!(result[0].imported);
    assert!(result[0].digest.starts_with("v2:"));
    assert_eq!(total(&target), original);
    let restored = Store::open(&target).unwrap();
    assert_eq!(
        restored
            .load_alert_settings()
            .unwrap()
            .unwrap()
            .threshold_mib,
        127
    );
    assert_eq!(
        restored
            .app_targets(day(), "Code.exe", Some(0), Some(0))
            .unwrap()[0]
            .app
            .as_deref(),
        Some("example.com")
    );
    let copy = directory.path().join("renamed-frozen.sqlite3");
    backup_database(&source.snapshot, &copy).unwrap();
    let repeat = merge_history(
        &target,
        &[HistorySource {
            snapshot: copy,
            source_key: source.source_key,
        }],
    )
    .unwrap();
    assert!(!repeat[0].imported);
    assert!(!repeat[0].targets_enriched);
    assert_eq!(total(&target), original);
    restored.check_integrity().unwrap();
}

#[test]
fn old_python_digest_can_enrich_targets_exactly_once_without_readding_history() {
    let directory = TempDir::new().unwrap();
    let target = directory.path().join("target.sqlite3");
    let source = make_source(directory.path(), "source");
    backup_database(&source.snapshot, &target).unwrap();
    let original = total(&target);
    {
        let db = Connection::open(&target).unwrap();
        db.execute_batch("DELETE FROM targets;DELETE FROM minute_targets;CREATE TABLE history_imports(source_key TEXT PRIMARY KEY,digest TEXT NOT NULL,imported_at REAL NOT NULL);").unwrap();
        db.execute(
            "INSERT INTO history_imports VALUES(?1,?2,42.0)",
            params![key(&source), fixture()["legacy_digest"].as_str().unwrap()],
        )
        .unwrap();
    }
    let results = merge_history(&target, std::slice::from_ref(&source)).unwrap();
    assert!(results[0].targets_enriched);
    assert!(!results[0].imported);
    assert_eq!(total(&target), original);
    let rows = Store::open(&target)
        .unwrap()
        .app_targets(day(), "Code.exe", Some(0), Some(0))
        .unwrap();
    assert_eq!(rows[0].uploaded, 8);
    let results = merge_history(&target, &[source]).unwrap();
    assert!(!results[0].targets_enriched);
    assert_eq!(total(&target), original);
    let db = Connection::open(&target).unwrap();
    assert_eq!(
        db.query_row("SELECT imported_at FROM history_imports", [], |r| r
            .get::<_, f64>(0))
            .unwrap(),
        42.0
    );
}

#[test]
fn legacy_digest_is_byte_compatible_with_python_and_source_is_read_only() {
    let directory = TempDir::new().unwrap();
    let target = directory.path().join("target.sqlite3");
    let source = make_source(directory.path(), "source");
    Connection::open(&source.snapshot)
        .unwrap()
        .execute_batch("DELETE FROM targets;DELETE FROM minute_targets;")
        .unwrap();
    let before = std::fs::read(&source.snapshot).unwrap();
    let results = merge_history(&target, &[source.clone()]).unwrap();
    assert_eq!(
        results[0].digest,
        fixture()["legacy_digest"].as_str().unwrap()
    );
    assert_eq!(std::fs::read(&source.snapshot).unwrap(), before);
    assert_eq!(
        Store::open(&target)
            .unwrap()
            .app_targets(day(), "Code.exe", Some(0), Some(0))
            .unwrap()[0]
            .app
            .as_deref(),
        Some("历史未记录域名")
    );
}

#[test]
fn changed_source_rejects_and_rolls_back_other_sources_in_same_batch() {
    let directory = TempDir::new().unwrap();
    let target = directory.path().join("target.sqlite3");
    let source = make_source(directory.path(), "source");
    merge_history(&target, std::slice::from_ref(&source)).unwrap();
    let before = total(&target);
    Connection::open(&source.snapshot)
        .unwrap()
        .execute("UPDATE apps SET uploaded=uploaded+1", [])
        .unwrap();
    let other = make_source(directory.path(), "other");
    assert!(merge_history(&target, &[other, source]).is_err());
    assert_eq!(total(&target), before);
    assert_eq!(
        Connection::open(&target)
            .unwrap()
            .query_row("SELECT COUNT(*) FROM history_imports", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
}

#[test]
fn inherited_recovery_ledger_is_preserved_and_overlap_is_rejected() {
    let directory = TempDir::new().unwrap();
    let target = directory.path().join("target.sqlite3");
    let source = make_source(directory.path(), "source");
    let inherited = directory
        .path()
        .join("earlier-original.sqlite3")
        .to_string_lossy()
        .into_owned();
    Connection::open(&source.snapshot).unwrap().execute_batch("CREATE TABLE history_imports(source_key TEXT PRIMARY KEY,digest TEXT NOT NULL,imported_at REAL NOT NULL);").unwrap();
    Connection::open(&source.snapshot)
        .unwrap()
        .execute(
            "INSERT INTO history_imports VALUES(?1,'older-digest',456.5)",
            [&inherited],
        )
        .unwrap();
    merge_history(&target, std::slice::from_ref(&source)).unwrap();
    assert_eq!(
        Connection::open(&target)
            .unwrap()
            .query_row(
                "SELECT imported_at FROM history_imports WHERE source_key=?1",
                [canonical_key(&inherited)],
                |r| r.get::<_, f64>(0)
            )
            .unwrap(),
        456.5
    );
    let another_target = directory.path().join("another-target.sqlite3");
    Store::open(&another_target).unwrap();
    let db = Connection::open(&another_target).unwrap();
    db.execute_batch("CREATE TABLE history_imports(source_key TEXT PRIMARY KEY,digest TEXT NOT NULL,imported_at REAL NOT NULL);").unwrap();
    db.execute(
        "INSERT INTO history_imports VALUES(?1,'older-digest',456.5)",
        [&inherited],
    )
    .unwrap();
    drop(db);
    assert!(merge_history(&another_target, &[source]).is_err());
    assert_eq!(
        Connection::open(another_target)
            .unwrap()
            .query_row("SELECT COUNT(*) FROM apps", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[test]
fn overlapping_sources_sum_without_reconciling_and_target_settings_win() {
    let directory = TempDir::new().unwrap();
    let target = directory.path().join("target.sqlite3");
    let first = make_source(directory.path(), "first");
    let second = make_source(directory.path(), "second");
    let store = Store::open(&target).unwrap();
    store
        .save_alert_settings(&monitor_store::AlertSettings {
            enabled: false,
            threshold_mib: 999,
        })
        .unwrap();
    Connection::open(&second.snapshot)
        .unwrap()
        .execute("UPDATE checkpoint SET sampled_at=sampled_at+100", [])
        .unwrap();
    let first_checkpoint = Connection::open(&first.snapshot)
        .unwrap()
        .query_row("SELECT sampled_at FROM checkpoint", [], |r| {
            r.get::<_, f64>(0)
        })
        .unwrap();
    let original = total(&first.snapshot);
    merge_history(&target, &[first, second]).unwrap();
    assert_eq!(total(&target), (original.0 * 2, original.1 * 2));
    assert_eq!(
        store.load_alert_settings().unwrap().unwrap().threshold_mib,
        999
    );
    assert_eq!(store.last_sample().unwrap(), Some(first_checkpoint + 100.0));
}

#[test]
fn rejects_live_source_self_restore_broken_associations_and_invalid_batch() {
    let directory = TempDir::new().unwrap();
    let target = directory.path().join("target.sqlite3");
    let source = make_source(directory.path(), "source");
    assert!(merge_history(&target, &[]).is_err());
    let live = HistorySource {
        source_key: source.snapshot.to_string_lossy().into_owned(),
        snapshot: source.snapshot.clone(),
    };
    assert!(merge_history(&target, &[live]).is_err());
    let self_source = HistorySource {
        source_key: target.to_string_lossy().into_owned(),
        snapshot: source.snapshot.clone(),
    };
    assert!(merge_history(&target, &[self_source]).is_err());
    Connection::open(&source.snapshot)
        .unwrap()
        .execute_batch(
            "PRAGMA foreign_keys=OFF;INSERT INTO minute_targets VALUES(0,'app','invalid',1,1);",
        )
        .unwrap();
    assert!(merge_history(&target, &[source]).is_err());
    assert!(!target.exists());
}

#[test]
fn pre_detail_database_recovers_without_mutating_source_or_backfilling_detail() {
    let directory = TempDir::new().unwrap();
    let target = directory.path().join("target.sqlite3");
    let snapshot = directory.path().join("old-frozen.sqlite3");
    Connection::open(&snapshot).unwrap().execute_batch("CREATE TABLE hours(day TEXT NOT NULL,hour INTEGER NOT NULL,samples INTEGER NOT NULL DEFAULT 0,
        gap_seconds REAL NOT NULL DEFAULT 0,unattributed_up INTEGER NOT NULL DEFAULT 0,unattributed_down INTEGER NOT NULL DEFAULT 0,
        unmatched_up INTEGER NOT NULL DEFAULT 0,unmatched_down INTEGER NOT NULL DEFAULT 0,PRIMARY KEY(day,hour));
        CREATE TABLE apps(day TEXT NOT NULL,hour INTEGER NOT NULL,app TEXT NOT NULL COLLATE NOCASE,uploaded INTEGER NOT NULL,downloaded INTEGER NOT NULL,PRIMARY KEY(day,hour,app));
        CREATE TABLE checkpoint(id INTEGER PRIMARY KEY,sampled_at REAL NOT NULL);
        INSERT INTO hours(day,hour,samples) VALUES('2026-11-12',10,2);
        INSERT INTO apps VALUES('2026-11-12',10,'Code.exe',11,33);
        INSERT INTO checkpoint VALUES(1,12345.5);").unwrap();
    let before = std::fs::read(&snapshot).unwrap();
    let source = HistorySource {
        snapshot: snapshot.clone(),
        source_key: directory
            .path()
            .join("old-original.sqlite3")
            .to_string_lossy()
            .into_owned(),
    };
    merge_history(&target, &[source]).unwrap();
    assert_eq!(std::fs::read(snapshot).unwrap(), before);
    let restored = Store::open(&target).unwrap();
    let rows = restored.day_hours(day(), None, 3.5).unwrap();
    assert_eq!(
        (rows[10].uploaded, rows[10].downloaded, rows[10].samples),
        (11, 33, 2)
    );
    assert!(rows[10].legacy_coverage);
    assert_eq!(rows[10].covered_seconds, 0.0);
    assert_eq!(restored.last_sample().unwrap(), Some(12345.5));
    assert_eq!(
        restored
            .minute_detail(day(), 10, 0, None, 3.5)
            .unwrap()
            .0
            .samples,
        0
    );
    assert_eq!(
        restored.app_targets(day(), "Code.exe", None, None).unwrap()[0]
            .app
            .as_deref(),
        Some("历史未记录域名")
    );
}

// Simulate the original Python importer: it accepted aggregate A's traffic
// and its legacy digest, but omitted A's inherited B ledger and target tables.
fn legacy_aggregate(
    directory: &Path,
    with_targets: bool,
) -> (std::path::PathBuf, HistorySource, HistorySource) {
    let target = directory.join("target.sqlite3");
    let aggregate = make_source(directory, "aggregate-a");
    let nested = make_source(directory, "nested-b");
    Connection::open(&nested.snapshot)
        .unwrap()
        .execute_batch("DELETE FROM targets;DELETE FROM minute_targets;")
        .unwrap();
    let db = Connection::open(&aggregate.snapshot).unwrap();
    // A contains B's history plus an equally sized collection period of its
    // own. Only A's own period has target attribution in this old aggregate.
    db.execute_batch("UPDATE apps SET uploaded=uploaded*2,downloaded=downloaded*2;
        UPDATE minute_apps SET uploaded=uploaded*2,downloaded=downloaded*2;
        CREATE TABLE history_imports(source_key TEXT PRIMARY KEY,digest TEXT NOT NULL,imported_at REAL NOT NULL);").unwrap();
    db.execute(
        "INSERT INTO history_imports VALUES(?1,?2,456.5)",
        params![key(&nested), fixture()["legacy_digest"].as_str().unwrap()],
    )
    .unwrap();
    if !with_targets {
        db.execute_batch("DELETE FROM targets;DELETE FROM minute_targets;")
            .unwrap();
    }
    drop(db);
    let old_view = directory.join("old-python-view.sqlite3");
    backup_database(&aggregate.snapshot, &old_view).unwrap();
    Connection::open(&old_view)
        .unwrap()
        .execute_batch("DELETE FROM targets;DELETE FROM minute_targets;")
        .unwrap();
    merge_history(
        &target,
        &[HistorySource {
            snapshot: old_view,
            source_key: aggregate.source_key.clone(),
        }],
    )
    .unwrap();
    Connection::open(&target)
        .unwrap()
        .execute(
            "DELETE FROM history_imports WHERE source_key=?1",
            [key(&nested)],
        )
        .unwrap();
    (target, aggregate, nested)
}

#[test]
fn verifying_legacy_aggregate_inherits_missing_nested_ledger_without_recounting() {
    for with_targets in [false, true] {
        let directory = TempDir::new().unwrap();
        let (target, aggregate, nested) = legacy_aggregate(directory.path(), with_targets);
        let before = total(&target);
        let result = merge_history(&target, std::slice::from_ref(&aggregate)).unwrap();
        assert!(!result[0].imported);
        assert_eq!(result[0].targets_enriched, with_targets);
        assert_eq!(total(&target), before);
        let db = Connection::open(&target).unwrap();
        assert_eq!(
            db.query_row(
                "SELECT imported_at FROM history_imports WHERE source_key=?1",
                [key(&nested)],
                |row| row.get::<_, f64>(0)
            )
            .unwrap(),
            456.5
        );
        // Exercise exact-digest skipping too, after the v2 enrichment path.
        db.execute(
            "DELETE FROM history_imports WHERE source_key=?1",
            [key(&nested)],
        )
        .unwrap();
        drop(db);
        let result = merge_history(&target, &[aggregate]).unwrap();
        assert!(!result[0].targets_enriched);
        let result = merge_history(&target, &[nested]).unwrap();
        assert!(!result[0].imported);
        assert!(!result[0].targets_enriched);
        assert_eq!(total(&target), before);
    }
}

#[test]
fn inherited_digest_conflict_rejects_whole_batch_and_target_enrichment() {
    let directory = TempDir::new().unwrap();
    let (target, aggregate, nested) = legacy_aggregate(directory.path(), true);
    let db = Connection::open(&target).unwrap();
    db.execute(
        "INSERT INTO history_imports VALUES(?1,'conflicting-digest',999.0)",
        [key(&nested)],
    )
    .unwrap();
    drop(db);
    let before = total(&target);
    let other = make_source(directory.path(), "unrelated");
    assert!(merge_history(&target, &[other, aggregate]).is_err());
    assert_eq!(total(&target), before);
    let db = Connection::open(&target).unwrap();
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM targets", [], |row| row
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM history_imports", [], |row| row
            .get::<_, i64>(0))
            .unwrap(),
        2
    );
    assert_eq!(
        db.query_row(
            "SELECT digest FROM history_imports WHERE source_key=?1",
            [key(&nested)],
            |row| row.get::<_, String>(0)
        )
        .unwrap(),
        "conflicting-digest"
    );
}

#[test]
fn aggregate_containing_target_itself_is_rejected_before_target_writes() {
    let directory = TempDir::new().unwrap();
    let target = directory.path().join("target.sqlite3");
    let aggregate = make_source(directory.path(), "aggregate");
    backup_database(&aggregate.snapshot, &target).unwrap();
    let before = std::fs::read(&target).unwrap();
    let db = Connection::open(&aggregate.snapshot).unwrap();
    db.execute_batch("CREATE TABLE history_imports(source_key TEXT PRIMARY KEY,digest TEXT NOT NULL,imported_at REAL NOT NULL);").unwrap();
    let inherited = target.to_string_lossy().into_owned();
    #[cfg(windows)]
    let inherited = inherited.to_uppercase().replace('\\', "/");
    db.execute(
        "INSERT INTO history_imports VALUES(?1,'target-prior-snapshot',1.0)",
        [inherited],
    )
    .unwrap();
    drop(db);
    assert!(merge_history(&target, &[aggregate]).is_err());
    assert_eq!(std::fs::read(&target).unwrap(), before);
    assert!(!target.with_file_name("target.sqlite3-wal").exists());
    assert!(!target.with_file_name("target.sqlite3-shm").exists());
}
