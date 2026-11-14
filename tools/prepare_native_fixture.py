"""Prepare synthetic SQLite history for a separate native WebView acceptance run.

Run: python -B tools/prepare_native_fixture.py
No GUI/controller is started. Every invocation creates a new native-ui-* directory
under the task's D: cache and writes its own traffic.sqlite3 plus expected.json.
Keep this separate from verify_rust_runtime.py's fresh-sampling assertions.
"""
from contextlib import closing
from dataclasses import asdict
from datetime import datetime, time, timedelta, timezone
import hashlib
import json
from pathlib import Path
import sqlite3
import sys
import uuid

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT))
from clash_monitor.storage import Store  # noqa: E402

BASE = Path("D:/DevCache/ClashTrafficMonitorTauri")
APP = "fixture-browser.exe"
OTHER_APP = "fixture-editor.exe"


def stamp(day, hour, minute=0, second=0):
    return datetime.combine(day, time(hour, minute, second)).timestamp()


def zero_window(store, day, hour):
    start = stamp(day, hour)
    store.record(start, {}, (0, 0))
    store.record(start + 2, {}, (0, 0), covered_from=start, segment=start)


def insert_legacy(database, day, hour, app, uploaded, downloaded):
    # This represents history predating coverage and target collection. Do not
    # create minute rows or pretend old bytes have a measured sampling duration.
    with closing(sqlite3.connect(database)) as db, db:
        db.execute("INSERT INTO hours(day,hour,samples,legacy_coverage) VALUES(?,?,3,1)",
                   (str(day), hour))
        db.execute("INSERT INTO apps VALUES(?,?,?,?,?)", (str(day), hour, app, uploaded, downloaded))


def totals(rows):
    return (sum(row.uploaded for row in rows), sum(row.downloaded for row in rows))


def prepare():
    now = datetime.now()
    today = now.date()
    day = today - timedelta(days=1)
    zero_day = today - timedelta(days=2)
    missing_day = today - timedelta(days=3)
    legacy_day = today - timedelta(days=4)
    if not BASE.is_dir():
        raise RuntimeError(f"Task cache must already exist: {BASE}")
    directory = BASE / ("native-ui-" + datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%S%fZ")
                        + "-" + uuid.uuid4().hex[:8])
    directory.mkdir()  # Never reuse, empty, or overwrite an earlier fixture.
    database = directory / "traffic.sqlite3"
    store = Store(database)
    store.save_alert_settings(False, 1024)
    insert_legacy(database, legacy_day, 7, "fixture-legacy.exe", 2048, 8192)
    zero_window(store, zero_day, 10)
    zero_window(store, day, 1)
    insert_legacy(database, day, 3, APP, 64, 512)
    start = stamp(day, 9, 4, 57)
    store.record(start, {}, (0, 0))
    store.record(start + 2, {APP: (1024, 4096)}, (0, 0), covered_from=start, segment=start,
                 targets={(APP, "example.invalid"): (1024, 4096)})
    store.record(start + 4, {APP: (256, 8192)}, (0, 0), covered_from=start + 2, segment=start,
                 targets={(APP, "192.0.2.44"): (128, 4096), (APP, "目标未知"): (128, 4096)})
    store.record(start + 6, {OTHER_APP: (512, 2048)}, (0, 0), covered_from=start + 4, segment=start,
                 targets={(OTHER_APP, "2001:db8::1"): (512, 2048)})
    if now.hour > 0:
        zero_window(store, today, 0)
    # An honest zero-duration baseline keeps the fixture checkpoint recent.
    # The later independent native instance may continue sampling this hour.
    store.record(now.timestamp(), {}, (0, 0))

    day_rows = store.day_hours(day)
    today_rows = store.day_hours(today)
    zero_rows = store.day_hours(zero_day)
    legacy_rows = store.day_hours(legacy_day)
    minute_queries = []
    for minute in (0, 5, 10):
        traffic, apps = store.minute_detail(day, 9, minute)
        minute_queries.append(dict(day=str(day), hour=9, minute=minute,
                                   traffic=asdict(traffic), apps=[asdict(row) for row in apps]))
    target_queries = []
    for app in (APP, OTHER_APP):
        for hour, minute in ((None, None), (9, None), (9, 0), (9, 5)):
            rows = store.app_targets(day, app, hour, minute)
            target_queries.append(dict(day=str(day), app=app, hour=hour, minute=minute,
                                       rows=[asdict(row) for row in rows],
                                       uploaded=sum(row.uploaded for row in rows),
                                       downloaded=sum(row.downloaded for row in rows)))
    days, range_apps = store.range_traffic(today - timedelta(days=6), today)
    expected = dict(
        schema_version=1, synthetic=True, generated_at=now.isoformat(),
        data_dir=str(directory.resolve()), database=str(database.resolve()),
        note="Synthetic UI fixture only; reserved names/documentation IPs, no real traffic or credentials.",
        cases={
            "value": dict(day=str(day), hour=9, app=APP),
            "collected_zero": dict(day=str(day), hour=1),
            "uncollected": dict(day=str(day), hour=2),
            "legacy_coverage_unknown": dict(day=str(day), hour=3, app=APP),
            "five_minute_value": dict(day=str(day), hour=9, minute=0, app=APP),
            "five_minute_other_targets": dict(day=str(day), hour=9, minute=5, app=APP),
            "five_minute_uncollected": dict(day=str(day), hour=9, minute=10),
            "ipv6_target": dict(day=str(day), hour=9, minute=5, app=OTHER_APP),
            "daily_collected_zero": dict(day=str(zero_day)),
            "daily_uncollected": dict(day=str(missing_day)),
            "daily_legacy_unknown": dict(day=str(legacy_day)),
            "today_baseline": dict(day=str(today), hour=now.hour),
            "future": dict(day=str(today), hours=list(range(now.hour + 1, 24))),
        },
        day_hours={str(day): [asdict(row) for row in day_rows],
                   str(today): [asdict(row) for row in today_rows],
                   str(zero_day): [asdict(row) for row in zero_rows],
                   str(legacy_day): [asdict(row) for row in legacy_rows]},
        days=[asdict(row) for row in days], range_apps=[asdict(row) for row in range_apps],
        hour_apps=[asdict(row) for row in store.hour_apps(day, 9)],
        minutes=minute_queries, targets=target_queries, checkpoint=store.last_sample(),
    )
    if now.hour > 0:
        expected["cases"]["today_collected_zero"] = dict(day=str(today), hour=0)
    assert (day_rows[1].uploaded, day_rows[1].downloaded) == (0, 0)
    assert day_rows[1].samples == 2 and day_rows[1].covered_seconds == 2
    assert day_rows[2].samples == 0 and day_rows[2].covered_seconds == 0
    assert day_rows[3].legacy_coverage and day_rows[3].covered_seconds == 0
    assert (day_rows[9].uploaded, day_rows[9].downloaded) == (1792, 14336)
    assert max(day_rows, key=lambda row: row.uploaded + row.downloaded).hour == 9
    assert totals(store.app_targets(day, APP)) == (1344, 12800)
    assert {row.app for row in store.app_targets(day, APP)} == {
        "example.invalid", "192.0.2.44", "目标未知", "历史未记录域名"}
    assert totals(store.app_targets(day, APP, 9)) == (1280, 12288)
    assert totals(store.app_targets(day, APP, 9, 0)) == (1024, 4096)
    assert totals(store.app_targets(day, APP, 9, 5)) == (256, 8192)
    assert store.minute_detail(day, 9, 10)[0].samples == 0
    assert not store.minute_detail(day, 3, 0)[1]  # No invented legacy detail.
    assert not store.app_targets(day, APP, 9, 10)
    with closing(sqlite3.connect(database)) as db:
        assert db.execute("PRAGMA quick_check").fetchall() == [("ok",)]
        assert not db.execute("PRAGMA foreign_key_check").fetchall()
        db.execute("PRAGMA wal_checkpoint(TRUNCATE)")
        expected["table_counts"] = {table: db.execute(f"SELECT COUNT(*) FROM {table}").fetchone()[0]
                                    for table in ("hours", "apps", "minutes", "minute_apps",
                                                  "targets", "minute_targets", "checkpoint")}
    with database.open("rb") as source:
        expected["initial_database_sha256"] = hashlib.file_digest(source, "sha256").hexdigest()
    expected_path = directory / "expected.json"
    expected_path.write_text(json.dumps(expected, ensure_ascii=False, indent=2, default=str) + "\n", encoding="utf-8")
    print(json.dumps(dict(event="NATIVE_UI_FIXTURE_READY", data_dir=str(directory.resolve()),
                          database=str(database.resolve()), expected=str(expected_path.resolve()),
                          cases=expected["cases"], table_counts=expected["table_counts"],
                          initial_database_sha256=expected["initial_database_sha256"]),
                     ensure_ascii=False), flush=True)


if __name__ == "__main__":
    prepare()
