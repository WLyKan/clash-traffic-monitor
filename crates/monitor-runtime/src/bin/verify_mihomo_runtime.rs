//! Development-only console probe. Never shipped as the desktop application.
//! Requires an explicit authenticated loopback controller and a new D: task DB.
use monitor_core::{MihomoSource, SamplingState};
use monitor_runtime::paths::WriterLock;
use monitor_store::Store;
use rusqlite::{Connection, OpenFlags};
use serde_json::json;
use std::{
    io::{self, BufRead, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread,
    time::{Duration, Instant},
};

fn output(value: serde_json::Value) {
    println!("{value}");
    let _ = io::stdout().flush();
}

fn isolated_directory(path: &Path) -> Result<PathBuf, String> {
    if !cfg!(windows)
        || !path.is_absolute()
        || path.file_name().and_then(|n| n.to_str()) != Some("collector")
    {
        return Err("probe requires an absolute Windows task directory ending in collector".into());
    }
    let base =
        dunce::canonicalize("D:/DevCache/ClashTrafficMonitorTauri").map_err(|e| e.to_string())?;
    let parent = dunce::canonicalize(path.parent().ok_or("missing task root")?)
        .map_err(|e| e.to_string())?;
    if parent.parent() != Some(base.as_path())
        || !parent
            .file_name()
            .and_then(|s| s.to_str())
            .is_some_and(|s| s.starts_with("real-mihomo-probe-"))
    {
        return Err(
            "probe only accepts a new real-mihomo-probe-* task inside the D: task cache".into(),
        );
    }
    if std::fs::symlink_metadata(path).is_ok() {
        return Err("probe refuses an existing collector directory".into());
    }
    Ok(parent.join("collector"))
}

fn sum(db: &Connection, table: &str) -> Result<(i64, i64), String> {
    db.query_row(
        &format!("SELECT COALESCE(SUM(uploaded),0),COALESCE(SUM(downloaded),0) FROM {table}"),
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )
    .map_err(|e| e.to_string())
}

fn finish(store: &Store, samples: u64) -> Result<serde_json::Value, String> {
    store.check_integrity().map_err(|e| e.to_string())?;
    let db = Connection::open_with_flags(store.path(), OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|e| e.to_string())?;
    let uploaded_downloaded = sum(&db, "apps")?;
    for table in ["targets", "minute_apps", "minute_targets"] {
        if sum(&db, table)? != uploaded_downloaded {
            return Err(format!(
                "{table} aggregate does not conserve application totals"
            ));
        }
    }
    let hours: i64 = db
        .query_row("SELECT COALESCE(SUM(samples),0) FROM hours", [], |r| {
            r.get(0)
        })
        .map_err(|e| e.to_string())?;
    let minutes: i64 = db
        .query_row("SELECT COALESCE(SUM(samples),0) FROM minutes", [], |r| {
            r.get(0)
        })
        .map_err(|e| e.to_string())?;
    if hours != samples as i64 || minutes != hours {
        return Err("hour/minute checkpoint sample conservation failed".into());
    }
    for (app_table, target_table, keys) in [
        (
            "apps",
            "targets",
            "a.day=t.day AND a.hour=t.hour AND a.app=t.app",
        ),
        (
            "minute_apps",
            "minute_targets",
            "a.bucket=t.bucket AND a.app=t.app",
        ),
    ] {
        let group = if app_table == "apps" {
            "a.day,a.hour,a.app"
        } else {
            "a.bucket,a.app"
        };
        let query=format!("SELECT COUNT(*) FROM (SELECT a.uploaded,a.downloaded FROM {app_table} a LEFT JOIN {target_table} t ON {keys} GROUP BY {group} HAVING a.uploaded!=COALESCE(SUM(t.uploaded),0) OR a.downloaded!=COALESCE(SUM(t.downloaded),0))");
        let mismatches: i64 = db
            .query_row(&query, [], |r| r.get(0))
            .map_err(|e| e.to_string())?;
        if mismatches != 0 {
            return Err(format!("{app_table} target conservation failed"));
        }
    }
    let mut query = db
        .prepare("SELECT app,SUM(uploaded),SUM(downloaded) FROM apps GROUP BY app ORDER BY app")
        .map_err(|e| e.to_string())?;
    let apps=query.query_map([],|r|Ok(json!({"app":r.get::<_,String>(0)?,"uploaded":r.get::<_,i64>(1)?,"downloaded":r.get::<_,i64>(2)?})))
        .map_err(|e|e.to_string())?.collect::<Result<Vec<_>,_>>().map_err(|e|e.to_string())?;
    Ok(
        json!({"event":"finished","samples":samples,"checkpoint":store.last_sample().map_err(|e|e.to_string())?,
        "apps":apps,"proxy_uploaded":uploaded_downloaded.0,"proxy_downloaded":uploaded_downloaded.1,
        "integrity":"ok","hour_target_conservation":true,"minute_target_conservation":true,"hour_minute_conservation":true}),
    )
}

fn run() -> Result<(), String> {
    let mut args = std::env::args_os().skip(1);
    let (mut controller, mut directory, mut seconds) = (None, None, 45u64);
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("--controller") => {
                controller = Some(
                    args.next()
                        .ok_or("--controller missing value")?
                        .into_string()
                        .map_err(|_| "invalid controller")?,
                )
            }
            Some("--data-dir") => {
                directory = Some(PathBuf::from(
                    args.next().ok_or("--data-dir missing value")?,
                ))
            }
            Some("--seconds") => {
                seconds = args
                    .next()
                    .ok_or("--seconds missing value")?
                    .to_str()
                    .ok_or("invalid seconds")?
                    .parse()
                    .map_err(|_| "invalid seconds")?
            }
            _ => return Err("probe accepts only --controller, --data-dir and --seconds".into()),
        }
    }
    if !(2..=120).contains(&seconds) {
        return Err("probe duration must be 2..120 seconds".into());
    }
    let controller = controller
        .ok_or("explicit --controller is required; default production pipes are forbidden")?;
    let directory = isolated_directory(&directory.ok_or("explicit --data-dir is required")?)?;
    let secret =
        std::env::var("CLASH_MONITOR_SECRET").map_err(|_| "explicit test secret is required")?;
    let mut source = MihomoSource::new(Some(&controller), Some(&secret))?;
    // Validate the controller before creating even an isolated SQLite file.
    let initial = source.sample()?;
    let lock = WriterLock::acquire(&directory)?;
    let mut store = Store::open(directory.join("traffic.sqlite3")).map_err(|e| e.to_string())?;
    let mut sampling = SamplingState::new(None, 0.1);
    let baseline = sampling.prepare(&initial);
    store.record(&baseline).map_err(|e| e.to_string())?;
    sampling.commit(initial, &baseline);
    let stopped = Arc::new(AtomicBool::new(false));
    let signal = stopped.clone();
    thread::spawn(move || {
        let _ = io::stdin().lock().lines().next();
        signal.store(true, Ordering::Release);
    });
    output(
        json!({"event":"ready","data_dir":directory,"database":store.path(),"pid":std::process::id()}),
    );
    let deadline = Instant::now() + Duration::from_secs(seconds);
    let mut samples = 1u64;
    while Instant::now() < deadline && !stopped.load(Ordering::Acquire) {
        let began = Instant::now();
        let current = source.sample()?;
        let record = sampling.prepare(&current);
        store.record(&record).map_err(|e| e.to_string())?;
        samples += 1;
        output(
            json!({"event":"sample","snapshot":current,"apps":record.apps,"targets":record.targets.iter().map(|((app,target),(up,down))|json!([app,target,up,down])).collect::<Vec<_>>(),
            "unknown_route":record.unattributed,"unmatched":record.unmatched,"covered_from":record.covered_from}),
        );
        sampling.commit(current, &record);
        thread::sleep(Duration::from_millis(100).saturating_sub(began.elapsed()));
    }
    let summary = finish(&store, samples)?;
    drop(store);
    drop(lock);
    output(summary);
    Ok(())
}

fn main() {
    if let Err(error) = run() {
        output(json!({"event":"error","message":error}));
        std::process::exit(1);
    }
}
