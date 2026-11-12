"""Export a reproducible Python-to-Rust contract using isolated SQLite only.

Run from the repository root: python tools/export_store_fixtures.py
The fixture stores local wall-clock inputs; Rust resolves these in its own
local zone, so the query comparison does not depend on the exporter's UTC offset.
"""
from contextlib import closing
from dataclasses import asdict
from datetime import date, datetime
import json
from pathlib import Path
import sqlite3
import sys
from tempfile import TemporaryDirectory

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT))
from clash_monitor.storage import Store  # noqa: E402
from clash_monitor.recovery import HistorySource, _read_snapshot  # noqa: E402


def boolean_quality(value):
    # Python dataclasses retain SQLite's 0/1 integer in this bool-annotated
    # field. The Rust/API model makes its declared boolean type explicit.
    if isinstance(value, dict):
        return {key: bool(item) if key == "legacy_coverage" else boolean_quality(item)
                for key, item in value.items()}
    if isinstance(value, list):
        return [boolean_quality(item) for item in value]
    return value


def export():
    records = [
        dict(local="2026-11-11T23:54:59", apps=[["Code.exe", 20, 40]],
             targets=[["Code.exe", "example.com", 20, 40]], unmatched=[0, 0]),
        dict(local="2026-11-11T23:59:59", apps=[["code.EXE", 7, 15], ["", 1, 2]],
             targets=[["code.EXE", "目标未知", 7, 15], ["", "[2001:db8::1]:443", 1, 2]],
             unmatched=[12, -5], covered_seconds=300, segment="2026-11-11T23:54:59"),
        dict(local="2026-11-12T00:00:01", apps=[["Code.exe", 8, 16]],
             targets=[["Code.exe", "example.com", 8, 16]], unmatched=[-8, 3],
             covered_seconds=2, segment="2026-11-11T23:54:59"),
        dict(local="2026-11-12T00:05:01", apps=[], targets=[], unmatched=[0, 0],
             gap_seconds=300),
        dict(local="2026-11-12T10:14:59", apps=[["Code.exe", 3, 9]], targets=[],
             unmatched=[0, 0], covered_seconds=2, segment="2026-11-12T10:14:57"),
    ]
    with TemporaryDirectory(prefix="rust-store-fixture-") as directory:
        path = Path(directory) / "snapshot.sqlite3"
        store = Store(path)
        store.save_alert_settings(True, 127)
        for record in records:
            stamp = datetime.fromisoformat(record["local"]).timestamp()
            store.record(stamp, {app or None: (up, down) for app, up, down in record["apps"]},
                         (0, 0), unmatched=tuple(record["unmatched"]),
                         covered_from=stamp-record["covered_seconds"] if "covered_seconds" in record else None,
                         gap_from=stamp-record["gap_seconds"] if "gap_seconds" in record else None,
                         segment=datetime.fromisoformat(record["segment"]).timestamp() if "segment" in record else None,
                         targets={(app or None, target): (up, down) for app, target, up, down in record["targets"]})
        first, second = date(2026, 11, 11), date(2026, 11, 12)
        days, apps = store.range_traffic(first, second)
        queries = dict(
            day_hours={str(day): [asdict(row) for row in store.day_hours(day)] for day in (first, second)},
            range_days=[asdict(row) for row in days], range_apps=[asdict(row) for row in apps],
            minutes=[dict(day=str(day), hour=hour, minute=minute,
                          traffic=asdict(store.minute_detail(day, hour, minute)[0]),
                          apps=[asdict(row) for row in store.minute_detail(day, hour, minute)[1]])
                     for day, hour, minute in ((first, 23, 50), (first, 23, 55), (second, 0, 0), (second, 0, 5), (second, 10, 10))],
            targets=[dict(day=str(day), hour=hour, minute=minute,
                          rows=[asdict(row) for row in store.app_targets(day, "Code.exe", hour, minute)])
                     for day, hour, minute in ((first, None, None), (second, None, None), (second, 0, None), (second, 0, 0), (second, 10, 10))],
        )
        with closing(sqlite3.connect(path)) as connection:
            dump = "\n".join(connection.iterdump())
        digest = _read_snapshot(HistorySource(path, str(Path(directory)/"original.sqlite3")), Path(directory)/"target.sqlite3").digest
        fixture = dict(records=records, expected=boolean_quality(queries), snapshot_sql=dump, legacy_digest=digest)
    target = ROOT / "tests/fixtures/rust-store/python-contract.json"
    target.parent.mkdir(parents=True, exist_ok=True)
    target.write_text(json.dumps(fixture, ensure_ascii=False, indent=2, default=str)+"\n", encoding="utf-8")
    print(target)


if __name__ == "__main__":
    export()
