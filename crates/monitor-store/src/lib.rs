//! SQLite storage compatible with the Python monitor's existing database.
//! All methods are synchronous; keep this object on the application's database
//! worker (or behind its mutex), never on the webview event thread.

pub mod recovery;
pub mod time;

use chrono::{Datelike, Duration, NaiveDate};
pub use monitor_core::{AlertSettings, SampleRecord};
use rusqlite::{
    params, params_from_iter, Connection, OpenFlags, OptionalExtension, TransactionBehavior,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    time::Duration as StdDuration,
};

pub use time::{hour_intervals, local_timestamp, minute_bucket, minute_buckets, minute_intervals};
pub const DETAIL_RETENTION: f64 = 14.0 * 86_400.0;
pub const MINUTE_BUCKET: i64 = 300;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("数据库操作失败：{0}")]
    Sql(#[from] rusqlite::Error),
    #[error("文件操作失败：{0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Invalid(String),
    #[error("无法编码历史摘要：{0}")]
    Json(#[from] serde_json::Error),
}
pub type Result<T> = std::result::Result<T, StoreError>;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct HourTraffic {
    pub hour: u32,
    pub uploaded: i64,
    pub downloaded: i64,
    pub unattributed_up: i64,
    pub unattributed_down: i64,
    pub samples: i64,
    pub gap_seconds: f64,
    pub covered_seconds: f64,
    pub legacy_coverage: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DayTraffic {
    pub day: NaiveDate,
    pub uploaded: i64,
    pub downloaded: i64,
    pub samples: i64,
    pub gap_seconds: f64,
    pub covered_seconds: f64,
    pub legacy_coverage: bool,
}
impl DayTraffic {
    pub fn empty(day: NaiveDate) -> Self {
        Self {
            day,
            uploaded: 0,
            downloaded: 0,
            samples: 0,
            gap_seconds: 0.0,
            covered_seconds: 0.0,
            legacy_coverage: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppTraffic {
    pub app: Option<String>,
    pub uploaded: i64,
    pub downloaded: i64,
}

pub struct Store {
    path: PathBuf,
    pub(crate) connection: Connection,
}

impl Store {
    /// Open/create and compatibly upgrade a database. The caller owns the
    /// data-directory writer lock before invoking this on a live database.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
        let mut connection = Connection::open(&path)?;
        connection.busy_timeout(StdDuration::from_secs(2))?;
        connection.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute_batch(include_str!("schema.sql"))?;
        let columns = columns(&transaction, "hours")?;
        if !columns.iter().any(|column| column == "covered_seconds") {
            transaction.execute_batch(
                "ALTER TABLE hours ADD COLUMN covered_seconds REAL NOT NULL DEFAULT 0;",
            )?;
        }
        if !columns.iter().any(|column| column == "legacy_coverage") {
            transaction.execute_batch(
                "ALTER TABLE hours ADD COLUMN legacy_coverage INTEGER NOT NULL DEFAULT 0;
                UPDATE hours SET legacy_coverage=1 WHERE samples>0;",
            )?;
        }
        transaction.commit()?;
        Ok(Self { path, connection })
    }

    /// Read-only opener: unlike `open`, this never creates directories,
    /// creates tables, changes PRAGMAs that write, or performs migrations.
    pub fn open_read_only(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let connection = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        connection.busy_timeout(StdDuration::from_secs(2))?;
        Ok(Self { path, connection })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn last_sample(&self) -> Result<Option<f64>> {
        Ok(self
            .connection
            .query_row("SELECT sampled_at FROM checkpoint WHERE id=1", [], |row| {
                row.get(0)
            })
            .optional()?)
    }

    pub fn load_alert_settings(&self) -> Result<Option<AlertSettings>> {
        Ok(self
            .connection
            .query_row(
                "SELECT enabled,threshold_mib FROM spike_settings WHERE id=1",
                [],
                |row| {
                    Ok(AlertSettings {
                        enabled: row.get::<_, bool>(0)?,
                        threshold_mib: row.get::<_, u64>(1)?,
                    })
                },
            )
            .optional()?)
    }

    pub fn save_alert_settings(&self, settings: &AlertSettings) -> Result<()> {
        if !(1..=1_000_000).contains(&settings.threshold_mib) {
            return Err(StoreError::Invalid("请输入 1–1000000 的整数阈值".into()));
        }
        self.connection.execute("INSERT INTO spike_settings VALUES(1,?1,?2)
            ON CONFLICT(id) DO UPDATE SET enabled=excluded.enabled,threshold_mib=excluded.threshold_mib",
            params![settings.enabled, settings.threshold_mib])?;
        Ok(())
    }

    /// Commit one entire sample, including its checkpoint, atomically.
    pub fn record(&mut self, sample: &SampleRecord) -> Result<()> {
        validate_record(sample)?;
        let local = local_timestamp(sample.sampled_at)?;
        use chrono::Timelike;
        let day = local.date_naive().to_string();
        let hour = local.hour();
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute("INSERT INTO hours(day,hour,samples,unattributed_up,unattributed_down,unmatched_up,unmatched_down)
            VALUES(?1,?2,1,?3,?4,?5,?6) ON CONFLICT(day,hour) DO UPDATE SET
            samples=samples+1,unattributed_up=unattributed_up+excluded.unattributed_up,
            unattributed_down=unattributed_down+excluded.unattributed_down,
            unmatched_up=unmatched_up+excluded.unmatched_up,unmatched_down=unmatched_down+excluded.unmatched_down",
            params![day,hour,sample.unattributed.0,sample.unattributed.1,sample.unmatched.0,sample.unmatched.1])?;
        for (app, (up, down)) in &sample.apps {
            transaction.execute("INSERT INTO apps VALUES(?1,?2,?3,?4,?5)
                ON CONFLICT(day,hour,app) DO UPDATE SET uploaded=uploaded+excluded.uploaded,downloaded=downloaded+excluded.downloaded",
                params![day,hour,app,up,down])?;
        }
        if let Some(start) = sample.gap_from {
            for (date, hour, seconds) in hour_intervals(start, sample.sampled_at)? {
                transaction.execute("INSERT INTO hours(day,hour,gap_seconds) VALUES(?1,?2,?3)
                    ON CONFLICT(day,hour) DO UPDATE SET gap_seconds=gap_seconds+excluded.gap_seconds",params![date,hour,seconds])?;
            }
        }
        if let Some(start) = sample.covered_from {
            for (date, hour, seconds) in hour_intervals(start, sample.sampled_at)? {
                transaction.execute("INSERT INTO hours(day,hour,covered_seconds) VALUES(?1,?2,?3)
                    ON CONFLICT(day,hour) DO UPDATE SET covered_seconds=covered_seconds+excluded.covered_seconds",params![date,hour,seconds])?;
            }
        }
        record_minutes(&transaction, sample)?;
        for ((app, target), (up, down)) in &sample.targets {
            transaction.execute("INSERT INTO targets VALUES(?1,?2,?3,?4,?5,?6)
                ON CONFLICT(day,hour,app,target) DO UPDATE SET uploaded=uploaded+excluded.uploaded,downloaded=downloaded+excluded.downloaded",
                params![day,hour,app,target,up,down])?;
            transaction.execute("INSERT INTO minute_targets VALUES(?1,?2,?3,?4,?5)
                ON CONFLICT(bucket,app,target) DO UPDATE SET uploaded=uploaded+excluded.uploaded,downloaded=downloaded+excluded.downloaded",
                params![minute_bucket(sample.sampled_at)?,app,target,up,down])?;
        }
        transaction.execute("INSERT INTO checkpoint VALUES(1,?1) ON CONFLICT(id) DO UPDATE SET sampled_at=excluded.sampled_at",[sample.sampled_at])?;
        transaction.commit()?;
        Ok(())
    }

    pub fn day_hours(
        &self,
        day: NaiveDate,
        as_of: Option<f64>,
        gap_tolerance: f64,
    ) -> Result<Vec<HourTraffic>> {
        let tx = self.connection.unchecked_transaction()?;
        let checkpoint: Option<f64> = if as_of.is_some() {
            tx.query_row("SELECT sampled_at FROM checkpoint WHERE id=1", [], |row| {
                row.get(0)
            })
            .optional()?
        } else {
            None
        };
        let mut result: Vec<_> = (0..24)
            .map(|hour| HourTraffic {
                hour,
                ..Default::default()
            })
            .collect();
        let mut statement=tx.prepare("SELECT h.hour,COALESCE(a.up,0),COALESCE(a.down,0),
            h.unattributed_up+MAX(0,h.unmatched_up),h.unattributed_down+MAX(0,h.unmatched_down),
            h.samples,h.gap_seconds,h.covered_seconds,h.legacy_coverage
            FROM hours h LEFT JOIN (SELECT hour,SUM(uploaded) up,SUM(downloaded) down FROM apps WHERE day=?1 GROUP BY hour) a ON h.hour=a.hour WHERE h.day=?1")?;
        for row in statement.query_map([day.to_string()], read_hour)? {
            let row = row?;
            let index = row.hour as usize;
            result[index] = row;
        }
        if let (Some(now), Some(checkpoint)) = (as_of, checkpoint) {
            if now - checkpoint > gap_tolerance {
                let day_start = time::local_day_start(day)?;
                let day_end = time::local_day_start(
                    day.checked_add_signed(Duration::days(1))
                        .ok_or_else(|| StoreError::Invalid("日期超出范围".into()))?,
                )?;
                for (_, hour, seconds) in
                    hour_intervals(checkpoint.max(day_start), now.min(day_end))?
                {
                    result[hour as usize].gap_seconds += seconds;
                }
            }
        }
        Ok(result)
    }

    pub fn range_traffic(
        &self,
        start: NaiveDate,
        end: NaiveDate,
    ) -> Result<(Vec<DayTraffic>, Vec<AppTraffic>)> {
        if end < start || (end - start).num_days() > 3660 {
            return Err(StoreError::Invalid("日期范围无效".into()));
        }
        let tx = self.connection.unchecked_transaction()?;
        let mut days = BTreeMap::new();
        let mut statement=tx.prepare("SELECT h.day,COALESCE(a.up,0),COALESCE(a.down,0),h.samples,h.gaps,h.coverage,h.legacy FROM
            (SELECT day,SUM(samples) samples,SUM(gap_seconds) gaps,SUM(covered_seconds) coverage,MAX(legacy_coverage) legacy FROM hours WHERE day BETWEEN ?1 AND ?2 GROUP BY day) h
            LEFT JOIN (SELECT day,SUM(uploaded) up,SUM(downloaded) down FROM apps WHERE day BETWEEN ?1 AND ?2 GROUP BY day) a ON h.day=a.day")?;
        let rows = statement.query_map(params![start.to_string(), end.to_string()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, f64>(4)?,
                row.get::<_, f64>(5)?,
                row.get::<_, bool>(6)?,
            ))
        })?;
        for row in rows {
            let (day, uploaded, downloaded, samples, gap_seconds, covered_seconds, legacy_coverage) =
                row?;
            let date = NaiveDate::parse_from_str(&day, "%Y-%m-%d")
                .map_err(|_| StoreError::Invalid("数据库日期无效".into()))?;
            days.insert(
                date,
                DayTraffic {
                    day: date,
                    uploaded,
                    downloaded,
                    samples,
                    gap_seconds,
                    covered_seconds,
                    legacy_coverage,
                },
            );
        }
        let apps=query_apps(&tx,"SELECT MIN(app),SUM(uploaded),SUM(downloaded) FROM apps WHERE day BETWEEN ?1 AND ?2 GROUP BY app ORDER BY SUM(uploaded+downloaded) DESC,app COLLATE NOCASE",params![start.to_string(),end.to_string()])?;
        let result = (0..=(end - start).num_days())
            .map(|n| {
                let date = start + Duration::days(n);
                days.remove(&date)
                    .unwrap_or_else(|| DayTraffic::empty(date))
            })
            .collect();
        Ok((result, apps))
    }

    pub fn hour_apps(&self, day: NaiveDate, hour: u32) -> Result<Vec<AppTraffic>> {
        validate_period(hour, None)?;
        query_apps(&self.connection,"SELECT app,uploaded,downloaded FROM apps WHERE day=?1 AND hour=?2 ORDER BY uploaded+downloaded DESC,app COLLATE NOCASE",params![day.to_string(),hour])
    }

    pub fn minute_detail(
        &self,
        day: NaiveDate,
        hour: u32,
        minute: u32,
        as_of: Option<f64>,
        gap_tolerance: f64,
    ) -> Result<(HourTraffic, Vec<AppTraffic>)> {
        validate_period(hour, Some(minute))?;
        let buckets: Vec<_> = minute_buckets(day, hour, minute)?
            .into_iter()
            .filter(|bucket| {
                as_of.map_or(true, |now| {
                    minute_bucket(now - DETAIL_RETENTION).map_or(false, |cutoff| *bucket >= cutoff)
                        && *bucket as f64 <= now
                })
            })
            .collect();
        if buckets.is_empty() {
            return Ok((
                HourTraffic {
                    hour,
                    ..Default::default()
                },
                vec![],
            ));
        }
        let first = buckets[0];
        let last = *buckets.last().unwrap();
        let tx = self.connection.unchecked_transaction()?;
        let checkpoint: Option<f64> = tx
            .query_row("SELECT sampled_at FROM checkpoint WHERE id=1", [], |r| {
                r.get(0)
            })
            .optional()?;
        let has_checkpoint_detail = if let Some(stamp) = checkpoint {
            tx.query_row(
                "SELECT 1 FROM minutes WHERE bucket=?1 AND samples>0",
                [minute_bucket(stamp)?],
                |r| r.get::<_, i64>(0),
            )
            .optional()?
            .is_some()
        } else {
            false
        };
        let mut row=tx.query_row("SELECT COALESCE(SUM(samples),0),COALESCE(SUM(covered_seconds),0),COALESCE(SUM(gap_seconds),0),
            COALESCE(SUM(unattributed_up),0),COALESCE(SUM(unattributed_down),0) FROM minutes WHERE bucket IN(?1,?2)",params![first,last],|r|Ok(HourTraffic{hour,samples:r.get(0)?,covered_seconds:r.get(1)?,gap_seconds:r.get(2)?,unattributed_up:r.get(3)?,unattributed_down:r.get(4)?,..Default::default()}))?;
        let residual:(i64,i64)=tx.query_row("SELECT MAX(0,COALESCE(SUM(up),0)),MAX(0,COALESCE(SUM(down),0)) FROM minute_residuals WHERE bucket IN(?1,?2)",params![first,last],|r|Ok((r.get(0)?,r.get(1)?)))?;
        row.unattributed_up += residual.0;
        row.unattributed_down += residual.1;
        let apps=query_apps(&tx,"SELECT MIN(app),SUM(uploaded),SUM(downloaded) FROM minute_apps WHERE bucket IN(?1,?2) GROUP BY app ORDER BY SUM(uploaded+downloaded) DESC,app COLLATE NOCASE",params![first,last])?;
        row.uploaded = apps.iter().map(|a| a.uploaded).sum();
        row.downloaded = apps.iter().map(|a| a.downloaded).sum();
        if let (Some(now), Some(last)) = (as_of, checkpoint) {
            if has_checkpoint_detail && now - last > gap_tolerance {
                row.gap_seconds += buckets
                    .iter()
                    .map(|bucket| {
                        (now.min((*bucket + MINUTE_BUCKET) as f64) - last.max(*bucket as f64))
                            .max(0.0)
                    })
                    .sum::<f64>();
            }
        }
        Ok((row, apps))
    }

    pub fn app_targets(
        &self,
        day: NaiveDate,
        app: &str,
        hour: Option<u32>,
        minute: Option<u32>,
    ) -> Result<Vec<AppTraffic>> {
        self.app_targets_at(day, app, hour, minute, None)
    }

    /// Apply the same query-time retention window as `minute_detail`, even
    /// when collection has been offline and no pruning write has occurred.
    /// Hourly and whole-day historical targets remain available.
    pub fn app_targets_at(
        &self,
        day: NaiveDate,
        app: &str,
        hour: Option<u32>,
        minute: Option<u32>,
        as_of: Option<f64>,
    ) -> Result<Vec<AppTraffic>> {
        if minute.is_some() && hour.is_none() {
            return Err(StoreError::Invalid("五分钟查询需要指定小时".into()));
        }
        if let Some(h) = hour {
            validate_period(h, minute)?;
        }
        let tx = self.connection.unchecked_transaction()?;
        let (table, app_table, condition, args): (
            String,
            String,
            String,
            Vec<rusqlite::types::Value>,
        ) = if let Some(minute) = minute {
            let buckets: Vec<_> = minute_buckets(day, hour.unwrap(), minute)?
                .into_iter()
                .filter(|bucket| {
                    as_of.map_or(true, |now| {
                        minute_bucket(now - DETAIL_RETENTION)
                            .map_or(false, |cutoff| *bucket >= cutoff)
                            && *bucket as f64 <= now
                    })
                })
                .collect();
            if buckets.is_empty() {
                return Ok(vec![]);
            }
            let condition = format!(
                "bucket IN ({}) AND app=?",
                vec!["?"; buckets.len()].join(",")
            );
            let mut args: Vec<_> = buckets.into_iter().map(Into::into).collect();
            args.push(app.to_string().into());
            (
                "minute_targets".into(),
                "minute_apps".into(),
                condition,
                args,
            )
        } else {
            let mut args = vec![day.to_string().into(), app.to_string().into()];
            let mut condition = "day=? AND app=?".to_string();
            if let Some(hour) = hour {
                condition.push_str(" AND hour=?");
                args.push((hour as i64).into());
            }
            ("targets".into(), "apps".into(), condition, args)
        };
        let mut rows=query_apps(&tx,&format!("SELECT target,SUM(uploaded),SUM(downloaded) FROM {table} WHERE {condition} GROUP BY target"),params_from_iter(args.iter()))?;
        let totals:(i64,i64)=tx.query_row(&format!("SELECT COALESCE(SUM(uploaded),0),COALESCE(SUM(downloaded),0) FROM {app_table} WHERE {condition}"),params_from_iter(args.iter()),|r|Ok((r.get(0)?,r.get(1)?)))?;
        let missing = (
            (totals.0 - rows.iter().map(|r| r.uploaded).sum::<i64>()).max(0),
            (totals.1 - rows.iter().map(|r| r.downloaded).sum::<i64>()).max(0),
        );
        if missing.0 != 0 || missing.1 != 0 {
            rows.push(AppTraffic {
                app: Some("历史未记录域名".into()),
                uploaded: missing.0,
                downloaded: missing.1,
            });
        }
        rows.sort_by(|a, b| {
            (b.uploaded + b.downloaded)
                .cmp(&(a.uploaded + a.downloaded))
                .then(a.app.cmp(&b.app))
        });
        Ok(rows)
    }

    pub fn check_integrity(&self) -> Result<()> {
        check_integrity(&self.connection)
    }

    pub fn backup_to(&self, target: impl AsRef<Path>) -> Result<()> {
        let target = target.as_ref();
        if target.exists() || target == self.path {
            return Err(StoreError::Invalid("备份目标必须是尚不存在的新文件".into()));
        }
        if let Some(parent) = target.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
        let mut destination = Connection::open(target)?;
        rusqlite::backup::Backup::new(&self.connection, &mut destination)?.run_to_completion(
            256,
            StdDuration::from_millis(5),
            None,
        )?;
        check_integrity(&destination)
    }
}

fn read_hour(row: &rusqlite::Row<'_>) -> rusqlite::Result<HourTraffic> {
    Ok(HourTraffic {
        hour: row.get(0)?,
        uploaded: row.get(1)?,
        downloaded: row.get(2)?,
        unattributed_up: row.get(3)?,
        unattributed_down: row.get(4)?,
        samples: row.get(5)?,
        gap_seconds: row.get(6)?,
        covered_seconds: row.get(7)?,
        legacy_coverage: row.get(8)?,
    })
}
fn query_apps<P: rusqlite::Params>(
    db: &Connection,
    sql: &str,
    params: P,
) -> Result<Vec<AppTraffic>> {
    let mut statement = db.prepare(sql)?;
    let rows = statement
        .query_map(params, |row| {
            let name: String = row.get(0)?;
            Ok(AppTraffic {
                app: if name.is_empty() { None } else { Some(name) },
                uploaded: row.get(1)?,
                downloaded: row.get(2)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}
pub(crate) fn columns(db: &Connection, table: &str) -> Result<Vec<String>> {
    let mut statement = db.prepare(&format!("PRAGMA table_info({table})"))?;
    let result = statement
        .query_map([], |row| row.get(1))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(result)
}
pub(crate) fn check_integrity(db: &Connection) -> Result<()> {
    let check: String = db.query_row("PRAGMA quick_check", [], |r| r.get(0))?;
    if check != "ok"
        || db
            .prepare("PRAGMA foreign_key_check")?
            .query([])?
            .next()?
            .is_some()
    {
        return Err(StoreError::Invalid("数据库完整性检查失败".into()));
    }
    Ok(())
}
fn validate_period(hour: u32, minute: Option<u32>) -> Result<()> {
    if hour > 23 || minute.is_some_and(|m| m > 55 || m % 5 != 0) {
        return Err(StoreError::Invalid("小时或五分钟范围无效".into()));
    }
    Ok(())
}
fn validate_record(sample: &SampleRecord) -> Result<()> {
    if !sample.sampled_at.is_finite()
        || sample
            .covered_from
            .into_iter()
            .chain(sample.gap_from)
            .chain(sample.segment)
            .any(|v| !v.is_finite() || v > sample.sampled_at)
    {
        return Err(StoreError::Invalid("采样时间无效".into()));
    }
    if sample
        .apps
        .values()
        .chain(sample.targets.values())
        .any(|(up, down)| *up < 0 || *down < 0)
        || sample.unattributed.0 < 0
        || sample.unattributed.1 < 0
    {
        return Err(StoreError::Invalid("已确认流量不能为负数".into()));
    }
    Ok(())
}
fn record_minutes(db: &Connection, sample: &SampleRecord) -> Result<()> {
    let bucket = minute_bucket(sample.sampled_at)?;
    let cutoff = minute_bucket(sample.sampled_at - DETAIL_RETENTION)?;
    let previous = if let Some(start) = sample.gap_from {
        db.query_row(
            "SELECT 1 FROM minutes WHERE bucket=?1 AND samples>0",
            [minute_bucket(start)?],
            |r| r.get::<_, i64>(0),
        )
        .optional()?
        .is_some()
    } else {
        false
    };
    db.execute("DELETE FROM minutes WHERE bucket<?1", [cutoff])?;
    db.execute("INSERT INTO minutes(bucket,samples,unattributed_up,unattributed_down) VALUES(?1,1,?2,?3)
        ON CONFLICT(bucket) DO UPDATE SET samples=samples+1,unattributed_up=unattributed_up+excluded.unattributed_up,unattributed_down=unattributed_down+excluded.unattributed_down",params![bucket,sample.unattributed.0,sample.unattributed.1])?;
    for (app, (up, down)) in &sample.apps {
        db.execute("INSERT INTO minute_apps VALUES(?1,?2,?3,?4) ON CONFLICT(bucket,app) DO UPDATE SET uploaded=uploaded+excluded.uploaded,downloaded=downloaded+excluded.downloaded",params![bucket,app,up,down])?;
    }
    if let Some(start) = sample.covered_from {
        for (bucket, seconds) in minute_intervals(start.max(cutoff as f64), sample.sampled_at)? {
            db.execute("INSERT INTO minutes(bucket,covered_seconds) VALUES(?1,?2) ON CONFLICT(bucket) DO UPDATE SET covered_seconds=covered_seconds+excluded.covered_seconds",params![bucket,seconds])?;
        }
        if sample.unmatched != (0, 0) {
            reconcile_minutes(
                db,
                bucket,
                sample.segment.unwrap_or(start),
                sample.unmatched,
            )?;
        }
    } else if previous {
        for (bucket, seconds) in minute_intervals(
            sample.gap_from.unwrap().max(cutoff as f64),
            sample.sampled_at,
        )? {
            db.execute("INSERT INTO minutes(bucket,gap_seconds) VALUES(?1,?2) ON CONFLICT(bucket) DO UPDATE SET gap_seconds=gap_seconds+excluded.gap_seconds",params![bucket,seconds])?;
        }
    }
    Ok(())
}
fn offset_balance(previous: i64, current: i64) -> i64 {
    if previous > 0 && current < 0 {
        (-previous).max(current)
    } else if previous < 0 && current > 0 {
        (-previous).min(current)
    } else {
        0
    }
}
fn reconcile_minutes(
    db: &Connection,
    bucket: i64,
    segment: f64,
    unmatched: (i64, i64),
) -> Result<()> {
    let current:(i64,i64)=db.query_row("INSERT INTO minute_residuals VALUES(?1,?2,?3,?4)
        ON CONFLICT(bucket,segment) DO UPDATE SET up=up+excluded.up,down=down+excluded.down RETURNING up,down",params![bucket,segment,unmatched.0,unmatched.1],|r|Ok((r.get(0)?,r.get(1)?)))?;
    let previous: Option<(i64, i64)> = db
        .query_row(
            "SELECT up,down FROM minute_residuals WHERE bucket=?1 AND segment=?2",
            params![bucket - MINUTE_BUCKET, segment],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    if let Some(previous) = previous {
        let transfer = (
            offset_balance(previous.0, current.0),
            offset_balance(previous.1, current.1),
        );
        if transfer != (0, 0) {
            for (bucket, up, down) in [
                (bucket - MINUTE_BUCKET, transfer.0, transfer.1),
                (bucket, -transfer.0, -transfer.1),
            ] {
                db.execute("UPDATE minute_residuals SET up=up+?1,down=down+?2 WHERE bucket=?3 AND segment=?4",params![up,down,bucket,segment])?;
            }
            let before = local_timestamp((bucket - MINUTE_BUCKET) as f64)?;
            let after = local_timestamp(bucket as f64)?;
            use chrono::Timelike;
            if (before.date_naive(), before.hour()) != (after.date_naive(), after.hour()) {
                for (date, hour, up, down) in [
                    (before.date_naive(), before.hour(), transfer.0, transfer.1),
                    (after.date_naive(), after.hour(), -transfer.0, -transfer.1),
                ] {
                    db.execute("UPDATE hours SET unmatched_up=unmatched_up+?1,unmatched_down=unmatched_down+?2 WHERE day=?3 AND hour=?4",params![up,down,date.to_string(),hour])?;
                }
            }
        }
    }
    Ok(())
}

/// Start of the current calendar range; weeks start on the local Monday.
pub fn trend_start(index: u32, today: NaiveDate) -> Result<NaiveDate> {
    match index {
        1 => Ok(today - Duration::days(6)),
        2 => Ok(today - Duration::days(29)),
        3 => Ok(today - Duration::days(today.weekday().num_days_from_monday() as i64)),
        4 => today
            .with_day(1)
            .ok_or_else(|| StoreError::Invalid("日期无效".into())),
        _ => Err(StoreError::Invalid("趋势类型无效".into())),
    }
}
