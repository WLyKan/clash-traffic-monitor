from contextlib import closing
from dataclasses import dataclass, replace
from datetime import date, datetime, timedelta
from pathlib import Path
import sqlite3


@dataclass(frozen=True)
class HourTraffic:
    hour: int
    uploaded: int = 0
    downloaded: int = 0
    unattributed_up: int = 0
    unattributed_down: int = 0
    samples: int = 0
    gap_seconds: float = 0
    covered_seconds: float = 0
    legacy_coverage: bool = False


@dataclass(frozen=True)
class DayTraffic:
    day: date
    uploaded: int = 0
    downloaded: int = 0
    samples: int = 0
    gap_seconds: float = 0
    covered_seconds: float = 0
    legacy_coverage: bool = False


@dataclass(frozen=True)
class AppTraffic:
    app: str | None
    uploaded: int
    downloaded: int


class Store:
    def __init__(self, path: Path):
        self.path = path
        path.parent.mkdir(parents=True, exist_ok=True)
        with closing(self._connect()) as db, db:
            db.executescript("""
                PRAGMA journal_mode = WAL;
                CREATE TABLE IF NOT EXISTS hours (
                    day TEXT NOT NULL,
                    hour INTEGER NOT NULL CHECK (hour BETWEEN 0 AND 23),
                    samples INTEGER NOT NULL DEFAULT 0,
                    gap_seconds REAL NOT NULL DEFAULT 0,
                    unattributed_up INTEGER NOT NULL DEFAULT 0,
                    unattributed_down INTEGER NOT NULL DEFAULT 0,
                    unmatched_up INTEGER NOT NULL DEFAULT 0,
                    unmatched_down INTEGER NOT NULL DEFAULT 0,
                    PRIMARY KEY (day, hour)
                );
                CREATE TABLE IF NOT EXISTS apps (
                    day TEXT NOT NULL,
                    hour INTEGER NOT NULL CHECK (hour BETWEEN 0 AND 23),
                    app TEXT NOT NULL COLLATE NOCASE,
                    uploaded INTEGER NOT NULL,
                    downloaded INTEGER NOT NULL,
                    PRIMARY KEY (day, hour, app)
                );
                CREATE TABLE IF NOT EXISTS checkpoint (
                    id INTEGER PRIMARY KEY CHECK (id = 1),
                    sampled_at REAL NOT NULL
                );
            """)
            db.execute("BEGIN IMMEDIATE")
            columns = {row[1] for row in db.execute("PRAGMA table_info(hours)")}
            if "covered_seconds" not in columns:
                db.execute("ALTER TABLE hours ADD COLUMN covered_seconds REAL NOT NULL DEFAULT 0")
                db.execute("ALTER TABLE hours ADD COLUMN legacy_coverage INTEGER NOT NULL DEFAULT 0")
                db.execute("UPDATE hours SET legacy_coverage = 1 WHERE samples > 0")

    def _connect(self):
        return sqlite3.connect(self.path, timeout=2)

    def last_sample(self) -> float | None:
        with closing(self._connect()) as db:
            row = db.execute("SELECT sampled_at FROM checkpoint WHERE id = 1").fetchone()
        return row[0] if row else None

    def record(
        self,
        sampled_at: float,
        apps: dict[str | None, tuple[int, int]],
        unattributed: tuple[int, int],
        gap_from: float | None = None,
        *,
        unmatched: tuple[int, int] = (0, 0),
        covered_from: float | None = None,
    ) -> None:
        local = datetime.fromtimestamp(sampled_at)
        day, hour = local.date().isoformat(), local.hour
        with closing(self._connect()) as db, db:
            db.execute("""
                INSERT INTO hours (day, hour, samples, unattributed_up, unattributed_down, unmatched_up, unmatched_down)
                VALUES (?, ?, 1, ?, ?, ?, ?)
                ON CONFLICT(day, hour) DO UPDATE SET
                    samples = samples + 1,
                    unattributed_up = unattributed_up + excluded.unattributed_up,
                    unattributed_down = unattributed_down + excluded.unattributed_down,
                    unmatched_up = unmatched_up + excluded.unmatched_up,
                    unmatched_down = unmatched_down + excluded.unmatched_down
            """, (day, hour, *unattributed, *unmatched))
            db.executemany("""
                INSERT INTO apps (day, hour, app, uploaded, downloaded) VALUES (?, ?, ?, ?, ?)
                ON CONFLICT(day, hour, app) DO UPDATE SET
                    uploaded = uploaded + excluded.uploaded,
                    downloaded = downloaded + excluded.downloaded
            """, ((day, hour, app or "", up, down) for app, (up, down) in apps.items()))
            if gap_from is not None:
                db.executemany("""
                    INSERT INTO hours (day, hour, gap_seconds) VALUES (?, ?, ?)
                    ON CONFLICT(day, hour) DO UPDATE SET
                        gap_seconds = gap_seconds + excluded.gap_seconds
                """, _interval_hours(gap_from, sampled_at))
            if covered_from is not None:
                db.executemany("""
                    INSERT INTO hours (day, hour, covered_seconds) VALUES (?, ?, ?)
                    ON CONFLICT(day, hour) DO UPDATE SET
                        covered_seconds = covered_seconds + excluded.covered_seconds
                """, _interval_hours(covered_from, sampled_at))
            db.execute("""
                INSERT INTO checkpoint (id, sampled_at) VALUES (1, ?)
                ON CONFLICT(id) DO UPDATE SET sampled_at = excluded.sampled_at
            """, (sampled_at,))

    def day_hours(
        self, day: date, *, as_of: float | None = None, gap_tolerance: float = 3.5,
    ) -> list[HourTraffic]:
        with closing(self._connect()) as db:
            db.execute("BEGIN")
            checkpoint = db.execute("SELECT sampled_at FROM checkpoint WHERE id = 1").fetchone() if as_of is not None else None
            rows = db.execute("""
                SELECT h.hour, COALESCE(a.up, 0), COALESCE(a.down, 0),
                       h.unattributed_up + MAX(0, h.unmatched_up),
                       h.unattributed_down + MAX(0, h.unmatched_down), h.samples, h.gap_seconds,
                       h.covered_seconds, h.legacy_coverage
                FROM hours h LEFT JOIN (
                    SELECT hour, SUM(uploaded) AS up, SUM(downloaded) AS down
                    FROM apps WHERE day = ? GROUP BY hour
                ) a ON h.hour = a.hour
                WHERE h.day = ?
            """, (day.isoformat(), day.isoformat())).fetchall()
        by_hour = {row[0]: HourTraffic(*row) for row in rows}
        if checkpoint and as_of - checkpoint[0] > gap_tolerance:
            start = max(checkpoint[0], datetime.combine(day, datetime.min.time()).timestamp())
            end = min(as_of, datetime.combine(day + timedelta(days=1), datetime.min.time()).timestamp())
            for _, hour, seconds in _interval_hours(start, end):
                row = by_hour.get(hour, HourTraffic(hour))
                by_hour[hour] = replace(row, gap_seconds=row.gap_seconds + seconds)
        return [by_hour.get(hour, HourTraffic(hour)) for hour in range(24)]

    def range_traffic(self, start: date, end: date) -> tuple[list[DayTraffic], list[AppTraffic]]:
        bounds = (start.isoformat(), end.isoformat())
        with closing(self._connect()) as db:
            db.execute("BEGIN")
            daily = db.execute("""
                SELECT h.day, COALESCE(a.up, 0), COALESCE(a.down, 0),
                       h.samples, h.gaps, h.coverage, h.legacy
                FROM (
                    SELECT day, SUM(samples) AS samples, SUM(gap_seconds) AS gaps,
                           SUM(covered_seconds) AS coverage, MAX(legacy_coverage) AS legacy
                    FROM hours WHERE day BETWEEN ? AND ? GROUP BY day
                ) h LEFT JOIN (
                    SELECT day, SUM(uploaded) AS up, SUM(downloaded) AS down
                    FROM apps WHERE day BETWEEN ? AND ? GROUP BY day
                ) a ON h.day = a.day
            """, bounds + bounds).fetchall()
            apps = db.execute("""
                SELECT MIN(app), SUM(uploaded), SUM(downloaded)
                FROM apps WHERE day BETWEEN ? AND ? GROUP BY app
                ORDER BY SUM(uploaded + downloaded) DESC, app COLLATE NOCASE
            """, bounds).fetchall()
        by_day = {date.fromisoformat(day): DayTraffic(date.fromisoformat(day), *values) for day, *values in daily}
        days = [start + timedelta(days=offset) for offset in range((end - start).days + 1)]
        return [by_day.get(day, DayTraffic(day)) for day in days], [
            AppTraffic(app or None, uploaded, downloaded) for app, uploaded, downloaded in apps
        ]

    def hour_apps(self, day: date, hour: int) -> list[AppTraffic]:
        with closing(self._connect()) as db:
            rows = db.execute("""
                SELECT app, uploaded, downloaded FROM apps WHERE day = ? AND hour = ?
                ORDER BY uploaded + downloaded DESC, app COLLATE NOCASE
            """, (day.isoformat(), hour)).fetchall()
        return [AppTraffic(app or None, uploaded, downloaded) for app, uploaded, downloaded in rows]


def _interval_hours(start: float, end: float):
    while start < end:
        local = datetime.fromtimestamp(start)
        elapsed = local.minute * 60 + local.second + local.microsecond / 1_000_000
        boundary = min(end, start + 3600 - elapsed)
        if boundary <= start:
            break
        yield local.date().isoformat(), local.hour, boundary - start
        start = boundary
