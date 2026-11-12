//! Offline recovery of frozen SQLite snapshots. Callers must hold the target
//! directory's cross-version writer lock for the entire merge. Opening the new
//! app on an existing database never invokes this additive recovery operation.
use crate::{check_integrity, columns, Result, Store, StoreError};
use rusqlite::{
    params, params_from_iter,
    types::{Value, ValueRef},
    Connection, OpenFlags, TransactionBehavior,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Component, Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HistorySource {
    pub snapshot: PathBuf,
    pub source_key: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ImportResult {
    pub source_key: String,
    pub digest: String,
    pub imported: bool,
    /// An older Python import did not carry targets. A verified unchanged
    /// source can add that dimension exactly once without adding apps again.
    pub targets_enriched: bool,
    pub rows: BTreeMap<String, usize>,
    pub uploaded: i64,
    pub downloaded: i64,
}

struct Table {
    name: &'static str,
    keys: &'static [&'static str],
    values: &'static [&'static str],
}
const LEGACY: &[Table] = &[
    Table {
        name: "hours",
        keys: &["day", "hour"],
        values: &[
            "samples",
            "gap_seconds",
            "unattributed_up",
            "unattributed_down",
            "unmatched_up",
            "unmatched_down",
            "covered_seconds",
            "legacy_coverage",
        ],
    },
    Table {
        name: "apps",
        keys: &["day", "hour", "app"],
        values: &["uploaded", "downloaded"],
    },
    Table {
        name: "minutes",
        keys: &["bucket"],
        values: &[
            "samples",
            "covered_seconds",
            "gap_seconds",
            "unattributed_up",
            "unattributed_down",
        ],
    },
    Table {
        name: "minute_apps",
        keys: &["bucket", "app"],
        values: &["uploaded", "downloaded"],
    },
    Table {
        name: "minute_residuals",
        keys: &["bucket", "segment"],
        values: &["up", "down"],
    },
    Table {
        name: "checkpoint",
        keys: &["id"],
        values: &["sampled_at"],
    },
    Table {
        name: "spike_settings",
        keys: &["id"],
        values: &["enabled", "threshold_mib"],
    },
];
const TARGETS: &[Table] = &[
    Table {
        name: "targets",
        keys: &["day", "hour", "app", "target"],
        values: &["uploaded", "downloaded"],
    },
    Table {
        name: "minute_targets",
        keys: &["bucket", "app", "target"],
        values: &["uploaded", "downloaded"],
    },
];
struct Snapshot {
    source_key: String,
    digest: String,
    legacy_digest: String,
    tables: BTreeMap<String, Vec<Vec<Value>>>,
    imports: Vec<(String, String, f64)>,
}

/// Consistent online backup, including WAL-committed data. Destination must be
/// a new file: never overwrite the current database or an earlier backup.
pub fn backup_database(source: impl AsRef<Path>, target: impl AsRef<Path>) -> Result<()> {
    let source = source.as_ref();
    let target = target.as_ref();
    if target.exists() || same_path(source, target)? {
        return Err(StoreError::Invalid("备份目标必须是尚不存在的新文件".into()));
    }
    if let Some(parent) = target.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    let connection = Connection::open_with_flags(source, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let mut destination = Connection::open(target)?;
    rusqlite::backup::Backup::new(&connection, &mut destination)?.run_to_completion(
        256,
        Duration::from_millis(5),
        None,
    )?;
    check_integrity(&destination)
}

pub fn merge_history(
    target: impl AsRef<Path>,
    sources: &[HistorySource],
) -> Result<Vec<ImportResult>> {
    if sources.is_empty() {
        return Err(StoreError::Invalid("至少需要一份历史快照".into()));
    }
    let target = target.as_ref();
    let snapshots: Vec<_> = sources
        .iter()
        .map(|source| read_snapshot(source, target))
        .collect::<Result<_>>()?;
    let mut store = Store::open(target)?;
    let tx = store
        .connection
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    tx.execute_batch("CREATE TABLE IF NOT EXISTS history_imports(source_key TEXT PRIMARY KEY,digest TEXT NOT NULL,imported_at REAL NOT NULL);")?;
    let mut result = vec![];
    for snapshot in snapshots {
        let existing = lookup_import(&tx, &snapshot.source_key)?;
        let has_targets = TARGETS.iter().any(|t| !snapshot.tables[t.name].is_empty());
        let mut imported = false;
        let mut targets_enriched = false;
        match existing {
            Some((_, ref digest)) if digest == &snapshot.digest => {}
            Some((ref stored_key, ref digest))
                if digest == &snapshot.legacy_digest && has_targets =>
            {
                // Plain legacy hashes identify the old Python importer which
                // only ever inserted LEGACY tables, not either target table.
                for table in TARGETS {
                    merge_table(&tx, table, &snapshot.tables[table.name])?;
                }
                tx.execute(
                    "UPDATE history_imports SET digest=?1 WHERE source_key=?2",
                    params![snapshot.digest, stored_key],
                )?;
                targets_enriched = true;
            }
            Some(_) => {
                return Err(StoreError::Invalid(
                    "同一来源已导入过不同内容，拒绝再次全量累加".into(),
                ))
            }
            None => {
                // A recovered aggregate includes its earlier imported sources.
                // Importing it over any of those would count the source twice.
                for (key, _, _) in &snapshot.imports {
                    if lookup_import(&tx, key)?.is_some() {
                        return Err(StoreError::Invalid(
                            "快照包含已经恢复过的来源，拒绝重复累计".into(),
                        ));
                    }
                }
                for table in LEGACY.iter().chain(TARGETS) {
                    merge_table(&tx, table, &snapshot.tables[table.name])?;
                }
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map_err(|_| StoreError::Invalid("系统时间无效".into()))?
                    .as_secs_f64();
                tx.execute(
                    "INSERT INTO history_imports VALUES(?1,?2,?3)",
                    params![snapshot.source_key, snapshot.digest, now],
                )?;
                imported = true;
            }
        }
        // The legacy Python importer recorded only the aggregate's own
        // provenance. Verifying that aggregate again must also inherit its
        // enclosed sources, even when no traffic rows need to be added.
        // Otherwise a later direct import of an enclosed source double counts.
        inherit_imports(&tx, &snapshot.imports)?;
        let total = |index: usize| {
            snapshot.tables["apps"]
                .iter()
                .map(|row| match row[row.len() - index] {
                    Value::Integer(value) => value,
                    _ => 0,
                })
                .sum()
        };
        result.push(ImportResult {
            source_key: snapshot.source_key,
            digest: snapshot.digest,
            imported,
            targets_enriched,
            rows: snapshot
                .tables
                .iter()
                .map(|(name, rows)| (name.clone(), rows.len()))
                .collect(),
            uploaded: total(2),
            downloaded: total(1),
        });
    }
    check_integrity(&tx)?;
    tx.commit()?;
    Ok(result)
}

fn inherit_imports(db: &Connection, imports: &[(String, String, f64)]) -> Result<()> {
    for (key, digest, at) in imports {
        let existing = lookup_import(db, key)?;
        match existing {
            Some((_, existing)) if existing == *digest => {}
            Some(_) => {
                return Err(StoreError::Invalid(
                    "快照继承来源与现有恢复账本内容冲突，拒绝导入".into(),
                ))
            }
            None => {
                db.execute(
                    "INSERT INTO history_imports VALUES(?1,?2,?3)",
                    params![key, digest, at],
                )?;
            }
        }
    }
    Ok(())
}

// Old/manual ledgers may spell a verified absolute path with different case
// or separators. Compare provenance lexically like Python's normcase/normpath,
// while preserving the existing ledger key and original import timestamp.
fn lookup_import(db: &Connection, key: &str) -> Result<Option<(String, String)>> {
    let mut query = db.prepare("SELECT source_key,digest FROM history_imports")?;
    let rows = query.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut found = None;
    for row in rows {
        let (stored_key, digest) = row?;
        if source_key(&stored_key)? == key {
            if found.is_some() {
                return Err(StoreError::Invalid(
                    "恢复账本包含同一物理来源的重复路径，拒绝导入".into(),
                ));
            }
            found = Some((stored_key, digest));
        }
    }
    Ok(found)
}

fn read_snapshot(source: &HistorySource, target: &Path) -> Result<Snapshot> {
    let key = source_key(&source.source_key)?;
    let target_key = source_key(&absolute_lexical(target)?.to_string_lossy())?;
    if key == target_key {
        return Err(StoreError::Invalid(
            "不能把目标数据库自身的历史快照再次累加回目标".into(),
        ));
    }
    let supplied = absolute_lexical(&source.snapshot)?;
    if source_key(&supplied.to_string_lossy())? == key {
        return Err(StoreError::Invalid(
            "请提供已冻结的备份快照，并指定原数据库路径".into(),
        ));
    }
    if !source.snapshot.is_file() {
        return Err(StoreError::Invalid("历史快照不存在".into()));
    }
    if Path::new(&source.source_key).exists()
        && same_path(&source.snapshot, Path::new(&source.source_key))?
    {
        return Err(StoreError::Invalid(
            "源快照与原数据库指向同一物理文件，请先创建一致性备份".into(),
        ));
    }
    if same_path(&source.snapshot, target)? {
        return Err(StoreError::Invalid("恢复目标不能同时作为源快照".into()));
    }
    let db = Connection::open_with_flags(&source.snapshot, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let tx = db.unchecked_transaction()?;
    check_integrity(&tx)?;
    let mut statement = tx.prepare("SELECT name FROM sqlite_master WHERE type='table'")?;
    let existing: BTreeSet<String> = statement
        .query_map([], |row| row.get(0))?
        .collect::<std::result::Result<_, _>>()?;
    if !["hours", "apps", "checkpoint"]
        .iter()
        .all(|name| existing.contains(*name))
    {
        return Err(StoreError::Invalid("历史快照缺少必要的流量表".into()));
    }
    let minutes = ["minutes", "minute_apps", "minute_residuals"]
        .iter()
        .filter(|name| existing.contains(**name))
        .count();
    if minutes != 0 && minutes != 3 {
        return Err(StoreError::Invalid("历史快照的分钟表不完整".into()));
    }
    if existing.contains("minute_targets") && !existing.contains("minutes") {
        return Err(StoreError::Invalid(
            "历史快照的目标分钟表缺少分钟主表".into(),
        ));
    }
    let mut tables = BTreeMap::new();
    for table in LEGACY.iter().chain(TARGETS) {
        let names: Vec<_> = table.keys.iter().chain(table.values).copied().collect();
        if !existing.contains(table.name) {
            tables.insert(table.name.to_string(), vec![]);
            continue;
        }
        let available = columns(&tx, table.name)?;
        let mut selections = vec![];
        for name in &names {
            if available.iter().any(|c| c == name) {
                selections.push((*name).to_string());
            } else if table.name == "hours" && *name == "covered_seconds" {
                selections.push("0 AS covered_seconds".into());
            } else if table.name == "hours" && *name == "legacy_coverage" {
                selections.push("(samples>0) AS legacy_coverage".into());
            } else {
                return Err(StoreError::Invalid(format!(
                    "历史快照的 {} 表缺少 {} 字段",
                    table.name, name
                )));
            }
        }
        let sql = format!(
            "SELECT {} FROM {} ORDER BY {}",
            selections.join(","),
            table.name,
            names.join(",")
        );
        let mut statement = tx.prepare(&sql)?;
        let rows = statement
            .query_map([], |row| {
                (0..names.len())
                    .map(|index| match row.get_ref(index)? {
                        ValueRef::Null => Ok(Value::Null),
                        ValueRef::Integer(value) => Ok(Value::Integer(value)),
                        ValueRef::Real(value) => Ok(Value::Real(value)),
                        ValueRef::Text(value) => {
                            Ok(Value::Text(String::from_utf8_lossy(value).into_owned()))
                        }
                        ValueRef::Blob(_) => Err(rusqlite::Error::InvalidQuery),
                    })
                    .collect::<rusqlite::Result<Vec<_>>>()
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        if rows
            .iter()
            .flatten()
            .any(|v| matches!(v, Value::Null) || matches!(v,Value::Real(x) if !x.is_finite()))
        {
            return Err(StoreError::Invalid("历史快照包含无效数值".into()));
        }
        tables.insert(table.name.to_string(), rows);
    }
    let imports = if existing.contains("history_imports") {
        let mut query = tx.prepare(
            "SELECT source_key,digest,imported_at FROM history_imports ORDER BY source_key",
        )?;
        let rows = query
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        rows
    } else {
        vec![]
    };
    let imports = imports
        .into_iter()
        .map(|(inherited, digest, at): (String, String, f64)| {
            let inherited = source_key(&inherited)?;
            if inherited == target_key {
                return Err(StoreError::Invalid(
                    "聚合快照已经包含目标数据库自身，拒绝再次累计".into(),
                ));
            }
            if inherited == key {
                return Err(StoreError::Invalid("快照包含循环恢复来源".into()));
            }
            if !at.is_finite() {
                return Err(StoreError::Invalid("快照恢复账本的导入时间无效".into()));
            }
            Ok((inherited, digest, at))
        })
        .collect::<Result<Vec<_>>>()?;
    let legacy_digest = digest_tables(&tables, false)?;
    let has_targets = TARGETS.iter().any(|t| !tables[t.name].is_empty());
    let digest = if has_targets {
        format!("v2:{}", digest_tables(&tables, true)?)
    } else {
        legacy_digest.clone()
    };
    Ok(Snapshot {
        source_key: key,
        digest,
        legacy_digest,
        tables,
        imports,
    })
}

fn merge_table(db: &Connection, table: &Table, rows: &[Vec<Value>]) -> Result<()> {
    if rows.is_empty() {
        return Ok(());
    }
    let names: Vec<_> = table.keys.iter().chain(table.values).copied().collect();
    let update = match table.name {
        "spike_settings" => "DO NOTHING".to_string(),
        "checkpoint" => {
            "DO UPDATE SET sampled_at=MAX(checkpoint.sampled_at,excluded.sampled_at)".to_string()
        }
        _ => format!(
            "DO UPDATE SET {}",
            table
                .values
                .iter()
                .map(|column| {
                    if *column == "legacy_coverage" {
                        format!("{column}=MAX(hours.{column},excluded.{column})")
                    } else {
                        format!("{column}={}.{column}+excluded.{column}", table.name)
                    }
                })
                .collect::<Vec<_>>()
                .join(",")
        ),
    };
    let sql = format!(
        "INSERT INTO {}({}) VALUES({}) ON CONFLICT({}) {}",
        table.name,
        names.join(","),
        vec!["?"; names.len()].join(","),
        table.keys.join(","),
        update
    );
    let mut statement = db.prepare(&sql)?;
    for row in rows {
        statement.execute(params_from_iter(row.iter()))?;
    }
    Ok(())
}

/// Match Python json.dumps(... ensure_ascii=False, sort_keys=True,
/// separators=(',',':'), allow_nan=False), including Python float formatting.
fn digest_tables(tables: &BTreeMap<String, Vec<Vec<Value>>>, targets: bool) -> Result<String> {
    let mut text = String::from("{");
    let mut first = true;
    for (name, rows) in tables {
        if !targets && TARGETS.iter().any(|table| table.name == name) {
            continue;
        }
        if !first {
            text.push(',');
        }
        first = false;
        text.push_str(&serde_json::to_string(name)?);
        text.push_str(":[");
        for (r, row) in rows.iter().enumerate() {
            if r > 0 {
                text.push(',');
            }
            text.push('[');
            for (c, value) in row.iter().enumerate() {
                if c > 0 {
                    text.push(',');
                }
                match value {
                    Value::Integer(value) => text.push_str(&value.to_string()),
                    Value::Real(value) => text.push_str(&python_float(*value)?),
                    Value::Text(value) => text.push_str(&serde_json::to_string(value)?),
                    _ => return Err(StoreError::Invalid("历史摘要不支持空值或二进制".into())),
                }
            }
            text.push(']');
        }
        text.push(']');
    }
    text.push('}');
    Ok(format!("{:x}", Sha256::digest(text.as_bytes())))
}

fn python_float(value: f64) -> Result<String> {
    if !value.is_finite() {
        return Err(StoreError::Invalid("摘要数值不是有限值".into()));
    }
    // Rust's Debug formatter and Python repr both emit shortest round-trip
    // decimals; Python switches to scientific notation below 1e-4 / at 1e16.
    let raw = format!("{value:?}");
    let (negative, raw) = if let Some(rest) = raw.strip_prefix('-') {
        ("-", rest)
    } else {
        ("", raw.as_str())
    };
    let (mantissa, explicit) = match raw.split_once('e') {
        Some((mantissa, exponent)) => (
            mantissa,
            exponent
                .parse::<i32>()
                .map_err(|_| StoreError::Invalid("数值指数无效".into()))?,
        ),
        None => (raw, 0),
    };
    let point = mantissa.find('.').unwrap_or(mantissa.len()) as i32;
    let digits: Vec<_> = mantissa.chars().filter(|c| *c != '.').collect();
    let first_nonzero = digits.iter().position(|c| *c != '0');
    let Some(first_nonzero) = first_nonzero else {
        return Ok(format!("{negative}0.0"));
    };
    let exponent = explicit + point - first_nonzero as i32 - 1;
    let mut significant: String = digits[first_nonzero..].iter().collect();
    while significant.len() > 1 && significant.ends_with('0') {
        significant.pop();
    }
    if !(-4..16).contains(&exponent) {
        let (first, rest) = significant.split_at(1);
        let mantissa = if rest.is_empty() {
            first.to_string()
        } else {
            format!("{first}.{rest}")
        };
        return Ok(format!(
            "{negative}{mantissa}e{}{exponent:02}",
            if exponent >= 0 { "+" } else { "-" },
            exponent = exponent.abs()
        ));
    }
    let position = exponent + 1;
    let value = if position <= 0 {
        format!("0.{}{}", "0".repeat((-position) as usize), significant)
    } else if position as usize >= significant.len() {
        format!(
            "{}{}.0",
            significant,
            "0".repeat(position as usize - significant.len())
        )
    } else {
        let (left, right) = significant.split_at(position as usize);
        format!("{left}.{right}")
    };
    Ok(format!("{negative}{value}"))
}

fn absolute_lexical(path: &Path) -> Result<PathBuf> {
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut result = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                result.pop();
            }
            other => result.push(other.as_os_str()),
        }
    }
    Ok(result)
}
fn source_key(path: &str) -> Result<String> {
    if !Path::new(path).is_absolute() {
        return Err(StoreError::Invalid(
            "source_key 必须是已核实的原数据库绝对路径".into(),
        ));
    }
    let path = absolute_lexical(Path::new(path))?
        .to_string_lossy()
        .into_owned();
    #[cfg(windows)]
    {
        Ok(path.replace('/', "\\").to_lowercase())
    }
    #[cfg(not(windows))]
    {
        Ok(path)
    }
}
fn same_path(first: &Path, second: &Path) -> Result<bool> {
    if source_key(&absolute_lexical(first)?.to_string_lossy())?
        == source_key(&absolute_lexical(second)?.to_string_lossy())?
    {
        return Ok(true);
    }
    if first.exists() && second.exists() {
        return Ok(same_file::is_same_file(first, second)?);
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn python_numeric_encoding() {
        for (value, expected) in [
            (1.0, "1.0"),
            (-0.0, "-0.0"),
            (0.00001, "1e-05"),
            (1e16, "1e+16"),
            (1e-4, "0.0001"),
            (1.234e18, "1.234e+18"),
            (1234.5, "1234.5"),
        ] {
            assert_eq!(python_float(value).unwrap(), expected);
        }
    }
}
