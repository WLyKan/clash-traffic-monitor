from contextlib import closing
from dataclasses import dataclass
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
                """, _gap_hours(gap_from, sampled_at))
            db.execute("""
                INSERT INTO checkpoint (id, sampled_at) VALUES (1, ?)
                ON CONFLICT(id) DO UPDATE SET sampled_at = excluded.sampled_at
            """, (sampled_at,))

    def day_hours(self, day: date) -> list[HourTraffic]:
        with closing(self._connect()) as db:
            rows = db.execute("""
                SELECT h.hour, COALESCE(a.up, 0), COALESCE(a.down, 0),
                       h.unattributed_up + MAX(0, h.unmatched_up),
                       h.unattributed_down + MAX(0, h.unmatched_down), h.samples, h.gap_seconds
                FROM hours h LEFT JOIN (
                    SELECT hour, SUM(uploaded) AS up, SUM(downloaded) AS down
                    FROM apps WHERE day = ? GROUP BY hour
                ) a ON h.hour = a.hour
                WHERE h.day = ?
            """, (day.isoformat(), day.isoformat())).fetchall()
        by_hour = {row[0]: HourTraffic(*row) for row in rows}
        return [by_hour.get(hour, HourTraffic(hour)) for hour in range(24)]

    def hour_apps(self, day: date, hour: int) -> list[AppTraffic]:
        with closing(self._connect()) as db:
            rows = db.execute("""
                SELECT app, uploaded, downloaded FROM apps WHERE day = ? AND hour = ?
                ORDER BY uploaded + downloaded DESC, app COLLATE NOCASE
            """, (day.isoformat(), hour)).fetchall()
        return [AppTraffic(app or None, uploaded, downloaded) for app, uploaded, downloaded in rows]


def _gap_hours(start: float, end: float):
    while start < end:
        local = datetime.fromtimestamp(start)
        next_hour = local.replace(minute=0, second=0, microsecond=0) + timedelta(hours=1)
        next_time = next_hour.timestamp()
        if next_time <= start:
            next_time = start + 3600
        boundary = min(end, next_time)
        yield local.date().isoformat(), local.hour, boundary - start
        start = boundary
