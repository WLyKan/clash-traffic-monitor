"""Offline, idempotent recovery from frozen SQLite history snapshots."""

from contextlib import closing
from dataclasses import dataclass
import hashlib
import json
import os
from pathlib import Path
import sqlite3
import time

from .storage import Store


class RecoveryError(RuntimeError):
    pass


@dataclass(frozen=True)
class HistorySource:
    snapshot: Path
    source_key: str


@dataclass(frozen=True)
class ImportResult:
    source_key: str
    digest: str
    imported: bool
    rows: dict[str, int]
    uploaded: int
    downloaded: int


@dataclass(frozen=True)
class _Snapshot:
    source_key: str
    digest: str
    tables: dict[str, tuple]


_TABLES = {
    "hours": (
        ("day", "hour"),
        ("samples", "gap_seconds", "unattributed_up", "unattributed_down", "unmatched_up", "unmatched_down", "covered_seconds", "legacy_coverage"),
    ),
    "apps": (("day", "hour", "app"), ("uploaded", "downloaded")),
    "minutes": (("bucket",), ("samples", "covered_seconds", "gap_seconds", "unattributed_up", "unattributed_down")),
    "minute_apps": (("bucket", "app"), ("uploaded", "downloaded")),
    "minute_residuals": (("bucket", "segment"), ("up", "down")),
    "checkpoint": (("id",), ("sampled_at",)),
    "spike_settings": (("id",), ("enabled", "threshold_mib")),
}


def _canonical(path: Path | str) -> str:
    return os.path.normcase(str(Path(path).expanduser().resolve()))


def _source_key(value: str) -> str:
    # Provenance is supplied by the caller. Resolving AppData can virtualize it
    # into a different Windows app-container path and collapse distinct sources.
    if not os.path.isabs(value):
        raise RecoveryError("source_key 必须是已经核实的原数据库绝对路径。")
    return os.path.normcase(os.path.normpath(value))


def _same_file(first: Path, second: Path) -> bool:
    if _canonical(first) == _canonical(second):
        return True
    return first.exists() and second.exists() and first.samefile(second)


def _read_snapshot(source: HistorySource, target: Path) -> _Snapshot:
    provided_path = Path(source.snapshot).expanduser()
    source_key = _source_key(source.source_key)
    if source_key == _source_key(str(target)):
        raise RecoveryError("不能把目标数据库自身的历史快照再次累加回目标。")
    provided_key = os.path.normcase(os.path.normpath(os.path.abspath(provided_path)))
    if provided_key == source_key:
        raise RecoveryError("请提供已冻结的备份快照，并用 source_key 指定原数据库路径。")
    path = provided_path.resolve()
    if not path.is_file():
        raise RecoveryError(f"历史快照不存在：{path}")
    if _same_file(path, target):
        raise RecoveryError("恢复目标不能同时作为源快照。")
    if os.path.normcase(os.path.normpath(str(path))) == source_key:
        raise RecoveryError("请提供已冻结的备份快照，并用 source_key 指定原数据库路径。")
    tables = {}
    try:
        with closing(sqlite3.connect(path.as_uri() + "?mode=ro", uri=True)) as db:
            db.execute("PRAGMA query_only = ON")
            db.execute("BEGIN")
            if db.execute("PRAGMA quick_check").fetchall() != [("ok",)]:
                raise RecoveryError(f"历史快照完整性检查失败：{path}")
            existing = {row[0] for row in db.execute("SELECT name FROM sqlite_master WHERE type = 'table'")}
            if not {"hours", "apps", "checkpoint"} <= existing:
                raise RecoveryError(f"历史快照缺少必要的流量表：{path}")
            minute_tables = {"minutes", "minute_apps", "minute_residuals"} & existing
            if minute_tables and len(minute_tables) != 3:
                raise RecoveryError(f"历史快照的分钟表不完整：{path}")
            for table, (keys, values) in _TABLES.items():
                columns = keys + values
                if table not in existing:
                    tables[table] = ()
                    continue
                available = {row[1] for row in db.execute(f"PRAGMA table_info({table})")}
                selections = []
                for column in columns:
                    if column in available:
                        selections.append(column)
                    elif table == "hours" and column == "covered_seconds":
                        selections.append("0 AS covered_seconds")
                    elif table == "hours" and column == "legacy_coverage":
                        selections.append("(samples > 0) AS legacy_coverage")
                    else:
                        raise RecoveryError(f"历史快照的 {table} 表缺少 {column} 字段。")
                tables[table] = tuple(db.execute(
                    f"SELECT {', '.join(selections)} FROM {table} ORDER BY {', '.join(columns)}"
                ))
            if db.execute("PRAGMA foreign_key_check").fetchone() is not None:
                raise RecoveryError(f"历史快照包含无效的分钟关联：{path}")
    except sqlite3.Error as error:
        raise RecoveryError(f"无法读取历史快照：{path}") from error
    digest = hashlib.sha256(json.dumps(
        tables, ensure_ascii=False, sort_keys=True, separators=(",", ":"), allow_nan=False,
    ).encode("utf-8")).hexdigest()
    return _Snapshot(source_key, digest, tables)


def _merge_table(db: sqlite3.Connection, table: str, rows: tuple) -> None:
    if not rows:
        return
    keys, values = _TABLES[table]
    columns = keys + values
    if table == "spike_settings":
        update = "DO NOTHING"
    elif table == "checkpoint":
        update = "DO UPDATE SET sampled_at = MAX(checkpoint.sampled_at, excluded.sampled_at)"
    else:
        assignments = []
        for column in values:
            if column == "legacy_coverage":
                assignments.append("legacy_coverage = MAX(hours.legacy_coverage, excluded.legacy_coverage)")
            else:
                assignments.append(f"{column} = {table}.{column} + excluded.{column}")
        update = "DO UPDATE SET " + ", ".join(assignments)
    db.executemany(
        f"INSERT INTO {table} ({', '.join(columns)}) VALUES ({', '.join('?' for _ in columns)}) "
        f"ON CONFLICT ({', '.join(keys)}) {update}",
        rows,
    )


def merge_history(target: Path, sources: list[HistorySource]) -> list[ImportResult]:
    """Merge non-overlapping frozen histories. The caller must stop collection and hold its writer lock."""
    if not sources:
        raise RecoveryError("至少需要一份历史快照。")
    target = Path(target).expanduser().resolve()
    snapshots = [_read_snapshot(source, target) for source in sources]
    Store(target)
    results = []
    with closing(sqlite3.connect(target, timeout=5)) as db, db:
        db.execute("PRAGMA foreign_keys = ON")
        db.execute("BEGIN IMMEDIATE")
        db.execute("""
            CREATE TABLE IF NOT EXISTS history_imports (
                source_key TEXT PRIMARY KEY,
                digest TEXT NOT NULL,
                imported_at REAL NOT NULL
            )
        """)
        for snapshot in snapshots:
            existing = db.execute(
                "SELECT digest FROM history_imports WHERE source_key = ?", (snapshot.source_key,),
            ).fetchone()
            if existing is not None and existing[0] != snapshot.digest:
                raise RecoveryError("同一来源已导入过不同内容，拒绝再次全量累加。请核对冻结快照。")
            imported = existing is None
            if imported:
                for table in _TABLES:
                    _merge_table(db, table, snapshot.tables[table])
                db.execute("INSERT INTO history_imports VALUES (?, ?, ?)", (
                    snapshot.source_key, snapshot.digest, time.time(),
                ))
            results.append(ImportResult(
                snapshot.source_key,
                snapshot.digest,
                imported,
                {table: len(rows) for table, rows in snapshot.tables.items()},
                sum(row[-2] for row in snapshot.tables["apps"]),
                sum(row[-1] for row in snapshot.tables["apps"]),
            ))
        if db.execute("PRAGMA foreign_key_check").fetchone() is not None:
            raise RecoveryError("恢复后数据关联检查失败，已回滚本次合并。")
        if db.execute("PRAGMA quick_check").fetchall() != [("ok",)]:
            raise RecoveryError("恢复后数据库完整性检查失败，已回滚本次合并。")
    return results
