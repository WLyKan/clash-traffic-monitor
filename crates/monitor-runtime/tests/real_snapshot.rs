//! Opt-in compatibility acceptance against a frozen, consistent real backup.
//! Only a newly copied database under D: TEMP is ever opened for writing.
#![cfg(windows)]

use chrono::{Duration, Local, NaiveDate, TimeZone, Timelike};
use monitor_core::{Connection as Flow, Route, SamplingState, Snapshot};
use monitor_store::Store;
use rusqlite::{types::Value, Connection, OpenFlags};
use std::{
    collections::BTreeMap,
    fs,
    os::windows::process::CommandExt,
    path::{Path, PathBuf},
    process::Command,
};

const APP: &str = "SyntheticMigration.exe";
const TARGET: &str = "test.invalid";

fn sha256(path: &Path) -> String {
    let system = PathBuf::from(std::env::var_os("SystemRoot").expect("Windows SystemRoot"));
    let output = Command::new(system.join("System32/certutil.exe"))
        .arg("-hashfile")
        .arg(path)
        .arg("SHA256")
        .creation_flags(0x0800_0000)
        .output()
        .expect("run system read-only SHA-256 utility");
    assert!(output.status.success(), "SHA-256 computation failed");
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|line| line.split_whitespace().collect::<String>())
        .find(|line| line.len() == 64 && line.bytes().all(|b| b.is_ascii_hexdigit()))
        .expect("SHA-256 digest missing")
        .to_ascii_lowercase()
}

fn source_path() -> Option<PathBuf> {
    let Some(path) = std::env::var_os("CLASH_COMPAT_SNAPSHOT") else {
        eprintln!("SKIP: set CLASH_COMPAT_SNAPSHOT to a frozen D: preflight backup for real-history migration acceptance");
        return None;
    };
    let path = dunce::canonicalize(PathBuf::from(path)).expect("frozen snapshot exists");
    let base =
        dunce::canonicalize("D:/DevCache/ClashTrafficMonitorTauri").expect("D: task cache exists");
    let parent = path.parent().expect("snapshot parent");
    assert!(
        path.file_name()
            .is_some_and(|name| name == "traffic.sqlite3"),
        "only the frozen snapshot filename is accepted"
    );
    assert!(
        parent.parent() == Some(base.as_path())
            && parent
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("preflight-")),
        "snapshot must physically belong to a D: task preflight directory"
    );
    assert!(
        !path.with_file_name("traffic.sqlite3-wal").exists(),
        "snapshot must have no WAL companion"
    );
    Some(path)
}

fn readonly(path: &Path, immutable: bool) -> Connection {
    let mut uri = url::Url::from_file_path(path).expect("absolute database file URI");
    uri.set_query(Some(if immutable {
        "mode=ro&immutable=1"
    } else {
        "mode=ro"
    }));
    Connection::open_with_flags(
        uri.as_str(),
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
    )
    .expect("read-only database connection")
}

fn quoted(identifier: &str) -> String {
    format!("\"{}\"", identifier.replace('"', "\"\""))
}

#[derive(Clone)]
struct Table {
    name: String,
    columns: Vec<String>,
    rows: Vec<Vec<Value>>,
}

fn rows(
    db: &Connection,
    name: &str,
    columns: &[String],
    exclude_synthetic: bool,
) -> Vec<Vec<Value>> {
    let names = columns
        .iter()
        .map(|name| quoted(name))
        .collect::<Vec<_>>()
        .join(",");
    let order = (1..=columns.len())
        .map(|n| n.to_string())
        .collect::<Vec<_>>()
        .join(",");
    let filter = if exclude_synthetic {
        " WHERE app!=?1 COLLATE NOCASE"
    } else {
        ""
    };
    let mut query = db
        .prepare(&format!(
            "SELECT {names} FROM {}{filter} ORDER BY {order}",
            quoted(name)
        ))
        .expect("read history rows");
    let read = |row: &rusqlite::Row<'_>| {
        (0..columns.len())
            .map(|index| row.get(index))
            .collect::<rusqlite::Result<Vec<Value>>>()
    };
    if exclude_synthetic {
        query
            .query_map([APP], read)
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    } else {
        query
            .query_map([], read)
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }
}

fn capture(db: &Connection) -> Vec<Table> {
    let names=db.prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name").unwrap()
        .query_map([],|row|row.get::<_,String>(0)).unwrap().collect::<Result<Vec<_>,_>>().unwrap();
    names
        .into_iter()
        .map(|name| {
            let columns = db
                .prepare(&format!("PRAGMA table_info({})", quoted(&name)))
                .unwrap()
                .query_map([], |row| row.get::<_, String>(1))
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap();
            Table {
                rows: rows(db, &name, &columns, false),
                name,
                columns,
            }
        })
        .collect()
}

fn day_bytes(db: &Connection) -> BTreeMap<NaiveDate, (i64, i64)> {
    db.prepare("SELECT day,SUM(uploaded),SUM(downloaded) FROM apps GROUP BY day ORDER BY day")
        .unwrap()
        .query_map([], |row| {
            let text: String = row.get(0)?;
            Ok((
                NaiveDate::parse_from_str(&text, "%Y-%m-%d").expect("valid historical date"),
                (row.get(1)?, row.get(2)?),
            ))
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

fn synthetic(time: f64, uploaded: i64, downloaded: i64, start: f64) -> Snapshot {
    Snapshot {
        sampled_at: time,
        completed_at: Some(time + 0.01),
        upload_total: uploaded,
        download_total: downloaded,
        generation: 1,
        connections: vec![Flow {
            id: "compatibility-only".into(),
            started_at: Some(start),
            app: Some(APP.into()),
            route: Route::Proxy,
            uploaded,
            downloaded,
            target: TARGET.into(),
        }],
    }
}

#[test]
fn frozen_real_history_migrates_and_accepts_new_samples_without_mutating_source() {
    let Some(source) = source_path() else { return };
    let source_hash = sha256(&source);
    let original = readonly(&source, true);
    let tables = capture(&original);
    let old_days = day_bytes(&original);
    assert_eq!(
        old_days.len(),
        4,
        "the designated acceptance fixture must contain four historical dates"
    );
    let ledger = tables
        .iter()
        .find(|table| table.name == "history_imports")
        .expect("recovery ledger retained");
    assert_eq!(
        ledger.rows.len(),
        2,
        "the designated acceptance fixture must contain both recovery ledger rows"
    );
    assert!(
        tables.iter().any(|table| table.name == "spike_settings"),
        "persisted alert settings required"
    );
    let checkpoint: f64 = original
        .query_row("SELECT sampled_at FROM checkpoint WHERE id=1", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert!(checkpoint.is_finite());
    let synthetic_count: i64 = original
        .query_row(
            "SELECT COUNT(*) FROM apps WHERE app=?1 COLLATE NOCASE",
            [APP],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        synthetic_count, 0,
        "synthetic acceptance app must be unique"
    );
    let old_count: usize = tables.iter().map(|table| table.rows.len()).sum();
    drop(original);

    let temp_base = dunce::canonicalize(std::env::temp_dir()).expect("D: task TEMP exists");
    assert!(
        temp_base
            .to_string_lossy()
            .to_ascii_lowercase()
            .starts_with("d:\\"),
        "all writable acceptance data must stay on D:"
    );
    let temp = tempfile::Builder::new()
        .prefix("real-snapshot-compat-")
        .tempdir_in(&temp_base)
        .unwrap();
    let destination = temp.path().join("traffic.sqlite3");
    assert!(destination.starts_with(temp.path()) && !destination.exists());
    fs::copy(&source, &destination).expect("copy frozen backup to new D: test directory");
    assert_eq!(
        sha256(&destination),
        source_hash,
        "copy hash differs before opening"
    );
    let mut store = Store::open(&destination).expect("compatible migration of isolated copy");
    let copied = readonly(&destination, false);
    for table in &tables {
        assert!(
            rows(&copied, &table.name, &table.columns, false) == table.rows,
            "migration changed existing table rows"
        );
    }
    assert_eq!(
        store.last_sample().unwrap(),
        Some(checkpoint),
        "opening must retain checkpoint exactly"
    );
    assert!(
        day_bytes(&copied) == old_days,
        "opening changed historical date totals"
    );
    drop(copied);

    // Use a distinct date so every old date's total remains directly comparable.
    let latest = old_days
        .keys()
        .last()
        .copied()
        .unwrap()
        .max(monitor_core::time::local_at(checkpoint).date_naive());
    let day = latest + Duration::days(1);
    let began = Local
        .from_local_datetime(&day.and_hms_opt(12, 0, 0).unwrap())
        .single()
        .expect("valid synthetic local noon")
        .timestamp() as f64;
    assert!(began > checkpoint);
    let mut sampling = SamplingState::new(Some(checkpoint), 1.0);
    for snapshot in [
        synthetic(began, 100, 200, began - 1.0),
        synthetic(began + 1.0, 111, 223, began - 1.0),
    ] {
        let record = sampling.prepare(&snapshot);
        store
            .record(&record)
            .expect("commit synthetic sample atomically");
        assert_eq!(
            store.last_sample().unwrap(),
            Some(snapshot.sampled_at),
            "checkpoint must commit with each new sample"
        );
        sampling.commit(snapshot, &record);
    }
    let after = readonly(&destination, false);
    let new_days = day_bytes(&after);
    for (date, total) in &old_days {
        assert!(
            new_days.get(date) == Some(total),
            "old per-day uploaded/downloaded totals changed"
        );
    }
    for table in &tables {
        match table.name.as_str() {
            "checkpoint" => {}
            "hours" | "minutes" => {
                // Restarting later legitimately extends gaps. Every existing
                // byte, sample and coverage field must remain unchanged.
                let keep: Vec<_> = table
                    .columns
                    .iter()
                    .enumerate()
                    .filter(|(_, name)| name.as_str() != "gap_seconds")
                    .map(|(index, _)| index)
                    .collect();
                let columns = keep
                    .iter()
                    .map(|index| table.columns[*index].clone())
                    .collect::<Vec<_>>();
                let retained = rows(&after, &table.name, &columns, false);
                for row in &table.rows {
                    let expected = keep
                        .iter()
                        .map(|index| row[*index].clone())
                        .collect::<Vec<_>>();
                    assert!(
                        retained.contains(&expected),
                        "existing history values were lost or changed"
                    );
                }
            }
            "apps" | "targets" | "minute_apps" | "minute_targets" => {
                assert!(
                    rows(&after, &table.name, &table.columns, true) == table.rows,
                    "old application or target rows changed"
                );
            }
            "minute_residuals" => {
                let retained = rows(&after, &table.name, &table.columns, false);
                assert!(
                    table.rows.iter().all(|row| retained.contains(row)),
                    "old residual history changed"
                );
            }
            _ => assert!(
                rows(&after, &table.name, &table.columns, false) == table.rows,
                "settings, ledger or other existing table changed"
            ),
        }
    }
    let expected = (11, 23);
    let hour = monitor_core::time::local_at(began + 1.0).hour();
    let hour_apps = store.hour_apps(day, hour).unwrap();
    assert_eq!(hour_apps.len(), 1);
    assert_eq!((hour_apps[0].uploaded, hour_apps[0].downloaded), expected);
    for (hour, minute) in [(None, None), (Some(hour), None), (Some(hour), Some(0))] {
        let targets = store
            .app_targets_at(day, APP, hour, minute, Some(began + 1.0))
            .unwrap();
        assert_eq!(targets.len(), 1);
        assert!(
            targets[0].app.as_deref() == Some(TARGET),
            "synthetic target identity changed"
        );
        assert_eq!((targets[0].uploaded, targets[0].downloaded), expected);
    }
    let (minute, apps) = store
        .minute_detail(day, hour, 0, Some(began + 1.0), 3.5)
        .unwrap();
    assert_eq!((minute.uploaded, minute.downloaded), expected);
    assert_eq!(apps.len(), 1);
    assert_eq!((apps[0].uploaded, apps[0].downloaded), expected);
    assert_eq!(store.last_sample().unwrap(), Some(began + 1.0));
    store
        .check_integrity()
        .expect("copied upgraded database integrity");
    let final_count: usize = capture(&after).iter().map(|table| table.rows.len()).sum();
    drop(after);
    drop(store);

    assert!(
        !source.with_file_name("traffic.sqlite3-wal").exists(),
        "source unexpectedly gained WAL"
    );
    assert_eq!(
        sha256(&source),
        source_hash,
        "frozen source must remain byte-identical"
    );
    eprintln!(
        "{}",
        serde_json::json!({"source_sha256":source_hash,"historical_date_count":old_days.len(),
        "source_table_count":tables.len(),"source_row_count":old_count,"migrated_row_count":final_count,
        "history_import_row_count":ledger.rows.len()})
    );
}
