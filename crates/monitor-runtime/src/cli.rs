//! CLI parsing and diagnostics stay independent of WebView initialization.
//! The immutable diagnostic snapshot deliberately never changes an existing
//! database or creates a missing directory. Online backups use SQLite backup.
use crate::paths::{default_data_dir, resolve_data_dir};
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde_json::{json, Value};
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Options {
    pub data_dir: Option<PathBuf>,
    pub start_hidden: bool,
    pub check: bool,
    pub diagnostics: bool,
    pub help: bool,
    pub version: bool,
}
impl Options {
    pub fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Self, String> {
        let mut result = Self::default();
        let mut args = args.into_iter();
        while let Some(arg) = args.next() {
            match arg.to_str() {
                Some("--data-dir") => {
                    let value = args.next().ok_or("--data-dir 缺少目录")?;
                    if value.to_string_lossy().starts_with('-') {
                        return Err(
                            "--data-dir 缺少目录；以减号开头的目录请使用 --data-dir=<目录>".into(),
                        );
                    }
                    result.data_dir = Some(PathBuf::from(value))
                }
                Some("--start-hidden") => result.start_hidden = true,
                Some("--check") => result.check = true,
                Some("--diagnostics") => result.diagnostics = true,
                Some("--help" | "-h") => result.help = true,
                Some("--version" | "-V") => result.version = true,
                Some(value) if value.starts_with("--data-dir=") && value.len() > 11 => {
                    result.data_dir = Some(value[11..].into())
                }
                _ => return Err("未知命令行参数，请使用 --help 查看支持的选项".into()),
            }
        }
        if result.check && result.diagnostics {
            return Err("--check 与 --diagnostics 不能同时使用".into());
        }
        if result
            .data_dir
            .as_ref()
            .is_some_and(|p| p.as_os_str().is_empty())
        {
            return Err("数据目录不能为空".into());
        }
        Ok(result)
    }
}
pub const HELP: &str = "Clash 流量监控\n--data-dir <目录>  自定义数据目录\n--start-hidden      仅显示托盘，后台继续采集\n--check             只读检查 Mihomo 连接及应用归因\n--diagnostics       只读输出物理目录、历史和启动命令 JSON\n--version           显示版本\n--help              显示帮助";

pub fn diagnostics(directory: &Path) -> Result<Value, String> {
    let directory = resolve_data_dir(Some(directory))?;
    let database = directory.join("traffic.sqlite3");
    let wal = directory.join("traffic.sqlite3-wal");
    let mut report = json!({
        "effective_data_dir":directory,"physical_data_dir":directory,
        "effective_database_path":database,"physical_database_path":database,
        "database_exists":database.is_file(),"wal_exists":wal.is_file(),
        "wal_bytes":wal.metadata().map(|m|m.len()).unwrap_or(0),
        "snapshot_mode":"immutable",
        "snapshot_note":"只读主数据库快照；尚未合入主数据库的 WAL 提交不包含在以下汇总中。",
        "default_data_dir":default_data_dir().ok(),
        "login_startup_command":crate::startup::command(&std::env::current_exe().map_err(|e|e.to_string())?,&directory),
        "days":[],"checkpoint":null,"runtime":"rust-tauri","version":env!("CARGO_PKG_VERSION")
    });
    if !database.is_file() {
        return Ok(report);
    }
    let read = || -> Result<(Value, Value), String> {
        // url::Url escapes Windows spaces, %, # and non-ASCII in SQLite's URI.
        let mut uri =
            url::Url::from_file_path(&database).map_err(|_| "数据库路径无效".to_string())?;
        uri.set_query(Some("mode=ro&immutable=1"));
        let db = Connection::open_with_flags(
            uri.as_str(),
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
        )
        .map_err(|e| e.to_string())?;
        let tables: Vec<String> = db
            .prepare("SELECT name FROM sqlite_master WHERE type='table'")
            .map_err(|e| e.to_string())?
            .query_map([], |row| row.get(0))
            .map_err(|e| e.to_string())?
            .collect::<Result<_, _>>()
            .map_err(|e| e.to_string())?;
        let mut days = std::collections::BTreeMap::<String, Value>::new();
        if tables.iter().any(|t| t == "hours") {
            let columns: Vec<String> = db
                .prepare("PRAGMA table_info(hours)")
                .map_err(|e| e.to_string())?
                .query_map([], |r| r.get(1))
                .map_err(|e| e.to_string())?
                .collect::<Result<_, _>>()
                .map_err(|e| e.to_string())?;
            let coverage = if columns.iter().any(|c| c == "covered_seconds") {
                "SUM(covered_seconds)"
            } else {
                "NULL"
            };
            let mut query = db
                .prepare(&format!(
                    "SELECT day,SUM(samples),SUM(gap_seconds),{coverage} FROM hours GROUP BY day"
                ))
                .map_err(|e| e.to_string())?;
            let mut rows = query.query([]).map_err(|e| e.to_string())?;
            while let Some(row) = rows.next().map_err(|e| e.to_string())? {
                let day: String = row.get(0).map_err(|e| e.to_string())?;
                days.insert(day.clone(),json!({"day":day,"samples":row.get::<_,i64>(1).map_err(|e|e.to_string())?,"gap_seconds":row.get::<_,f64>(2).map_err(|e|e.to_string())?,"covered_seconds":row.get::<_,Option<f64>>(3).map_err(|e|e.to_string())?,"uploaded":0,"downloaded":0}));
            }
        }
        if tables.iter().any(|t| t == "apps") {
            let mut query = db
                .prepare("SELECT day,SUM(uploaded),SUM(downloaded) FROM apps GROUP BY day")
                .map_err(|e| e.to_string())?;
            let mut rows = query.query([]).map_err(|e| e.to_string())?;
            while let Some(row) = rows.next().map_err(|e| e.to_string())? {
                let day: String = row.get(0).map_err(|e| e.to_string())?;
                let item = days.entry(day.clone()).or_insert_with(
                    || json!({"day":day,"samples":0,"gap_seconds":0,"covered_seconds":null}),
                );
                item["uploaded"] = json!(row.get::<_, i64>(1).map_err(|e| e.to_string())?);
                item["downloaded"] = json!(row.get::<_, i64>(2).map_err(|e| e.to_string())?);
            }
        }
        let checkpoint = if tables.iter().any(|t| t == "checkpoint") {
            db.query_row("SELECT sampled_at FROM checkpoint WHERE id=1",[],|r|r.get::<_,f64>(0)).optional().map_err(|e|e.to_string())?.map(|t|json!({"sampled_at":t,"local_time":monitor_core::time::local_at(t).to_rfc3339()})).unwrap_or(Value::Null)
        } else {
            Value::Null
        };
        Ok((json!(days.into_values().collect::<Vec<_>>()), checkpoint))
    };
    match read() {
        Ok((days, checkpoint)) => {
            report["days"] = days;
            report["checkpoint"] = checkpoint;
        }
        Err(_) => {
            report["database_error"] = json!("无法读取数据库快照，请检查文件完整性和读取权限。")
        }
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preserve_arguments_and_reject_invalid_modes() {
        let parsed = Options::parse(
            [
                "--start-hidden",
                "--data-dir",
                "C:/用户 data",
                "--diagnostics",
            ]
            .map(OsString::from),
        )
        .unwrap();
        assert!(parsed.start_hidden && parsed.diagnostics);
        assert_eq!(parsed.data_dir, Some("C:/用户 data".into()));
        assert!(Options::parse(["--check", "--diagnostics"].map(OsString::from)).is_err());
        assert!(Options::parse(["--data-dir"].map(OsString::from)).is_err());
        assert!(Options::parse(["--data-dir="].map(OsString::from)).is_err());
        assert!(Options::parse(["--data-dir", "--check"].map(OsString::from)).is_err());
        assert!(Options::parse(["--data-dir", "--diagnostics"].map(OsString::from)).is_err());
        assert_eq!(
            Options::parse(["--data-dir=--folder", "--diagnostics"].map(OsString::from))
                .unwrap()
                .data_dir,
            Some("--folder".into())
        );
    }
    #[test]
    fn diagnostics_missing_does_not_create_anything() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("missing");
        let report = diagnostics(&path).unwrap();
        assert_eq!(report["database_exists"], false);
        assert!(!path.exists());
    }
    #[test]
    fn diagnostics_legacy_schema_and_special_characters_read_only() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("数据 # %");
        std::fs::create_dir(&path).unwrap();
        let file = path.join("traffic.sqlite3");
        let db = Connection::open(&file).unwrap();
        db.execute_batch("CREATE TABLE hours(day TEXT,hour INTEGER,samples INTEGER,gap_seconds REAL);CREATE TABLE apps(day TEXT,hour INTEGER,app TEXT,uploaded INTEGER,downloaded INTEGER);CREATE TABLE checkpoint(id INTEGER,sampled_at REAL);INSERT INTO hours VALUES('2026-01-01',0,3,1.0);INSERT INTO apps VALUES('2026-01-01',0,'a',5,7);INSERT INTO checkpoint VALUES(1,1767225600);").unwrap();
        drop(db);
        let before = std::fs::read(&file).unwrap();
        let report = diagnostics(&path).unwrap();
        assert!(report.get("database_error").is_none());
        assert_eq!(report["days"][0]["uploaded"], 5);
        assert_eq!(report["days"][0]["covered_seconds"], Value::Null);
        assert_eq!(std::fs::read(&file).unwrap(), before);
        assert!(!path.join("collector.lock").exists());
    }
}
