from contextlib import closing
from dataclasses import replace
from datetime import datetime, timedelta
from pathlib import Path
from tempfile import TemporaryDirectory
import sqlite3
import unittest
from clash_monitor.source import Connection, Snapshot, target_name
from clash_monitor.collector import Collector, traffic_delta
from clash_monitor.storage import Store


class TargetTests(unittest.TestCase):
    def setUp(self):
        self.directory = self.enterContext(TemporaryDirectory())
        self.path = Path(self.directory) / "traffic.sqlite3"
        self.store = Store(self.path)

    def test_resolution(self):
        for metadata, expected in (
            ({"host": "EXAMPLE.COM."}, "example.com"),
            ({"host": "1.2.3.4", "sniffHost": "Sniff.Example."}, "sniff.example"),
            ({"host": "bad/path", "sniffHost": "valid.example"}, "valid.example"),
            ({"destinationIP": "1.2.3.4"}, "1.2.3.4"),
            ({"destinationIP": "2001:0db8::1"}, "2001:db8::1"),
            ({}, "目标未知"), ({"host": "x_y", "destinationIP": "bad"}, "目标未知"),
            ({"sniffHost": "SNI.Example"}, "sni.example"),
        ):
            with self.subTest(metadata=metadata):
                self.assertEqual(target_name(metadata), expected)

    def test_delta_name_change_first_seen_end_and_reconnect(self):
        c = Connection("a", 0, "app", "proxy", 10, 20, "1.2.3.4")
        previous = Snapshot(100, 10, 20, (c,))
        targets = {}
        apps, _, _ = traffic_delta(previous, Snapshot(101, 15, 27, (replace(c, uploaded=15, downloaded=27, target="example.com"),)), targets=targets)
        self.assertEqual(targets, {("app", "example.com"): (5, 7)})
        self.assertEqual(apps, {"app": (5, 7)})
        targets = {}
        traffic_delta(previous, Snapshot(101, 15, 27, (replace(c, started_at=100.5, uploaded=3, downloaded=4),)), targets=targets)
        self.assertEqual(targets, {("app", "1.2.3.4"): (3, 4)})
        targets = {}
        traffic_delta(previous, Snapshot(101, 15, 27, ()), targets=targets)
        self.assertEqual(targets, {})
        traffic_delta(Snapshot(100, 0, 0, ()), Snapshot(101, 10, 20, (c,)), targets=targets)
        self.assertEqual(targets, {})
        for route in ("direct", "unknown"):
            traffic_delta(previous, Snapshot(101, 15, 27, (replace(c, route=route, uploaded=15),)), targets=targets)
        self.assertEqual(targets, {})

    def test_collector_persists_equal_target_and_application_bytes(self):
        stamp = datetime.now().replace(second=0, microsecond=0).timestamp()
        base = Connection("a", stamp-10, "app", "proxy", 100, 200)
        snapshots = iter((Snapshot(stamp,100,200,(base,)),
                          Snapshot(stamp+1,105,207,(replace(base,uploaded=105,downloaded=207,target="example.com"),)),
                          Snapshot(stamp+2,110,214,(replace(base,uploaded=110,downloaded=214),))))
        class Source:
            def sample(self):
                return next(snapshots)
        collector = Collector(Source(),self.store)
        for _ in range(3):
            collector.poll()
        day = datetime.fromtimestamp(stamp).date()
        rows = self.store.app_targets(day,"app")
        self.assertEqual({r.app for r in rows},{"example.com","目标未知"})
        self.assertEqual((sum(r.uploaded for r in rows),sum(r.downloaded for r in rows)),(10,14))
        self.assertEqual(self.store.last_sample(),stamp+2)

    def test_hour_and_five_minute_totals_legacy_and_retention(self):
        now = datetime.now().replace(hour=11, minute=59, second=59, microsecond=0)
        for offset, target in ((0, "目标未知"), (2, "example.com"), (302, "2001:db8::1")):
            stamp = (now + timedelta(seconds=offset)).timestamp()
            self.store.record(stamp, {"app": (2, 3)}, (0,0), covered_from=stamp-1, targets={("app", target): (2,3)})
        day = now.date()
        self.assertEqual(sum(r.uploaded+r.downloaded for r in self.store.app_targets(day,"APP")), 15)
        self.assertEqual(sum(r.uploaded+r.downloaded for r in self.store.app_targets(day,"app",12)), 10)
        self.assertEqual(self.store.app_targets(day,"app",12,0)[0].app, "example.com")
        self.assertEqual(self.store.app_targets(day,"app",11,55)[0].app, "目标未知")
        self.store.record(now.timestamp(), {"app": (7,11)}, (0,0))
        legacy = next(r for r in self.store.app_targets(day,"app") if r.app == "历史未记录域名")
        self.assertEqual((legacy.uploaded,legacy.downloaded),(7,11))
        self.store.record((now+timedelta(days=15)).timestamp(), {}, (0,0))
        self.assertEqual(self.store.app_targets(day,"app",12,0), [])
        self.assertTrue(self.store.app_targets(day,"app"))

    def test_upgrade_preserves_existing_tables_settings_checkpoint_and_ledger(self):
        self.store.record(datetime.now().timestamp(), {"old.exe":(2,9)}, (0,0))
        self.store.save_alert_settings(True, 123)
        with closing(sqlite3.connect(self.path)) as db, db:
            db.execute("CREATE TABLE history_imports (source TEXT, fingerprint TEXT)")
            db.execute("INSERT INTO history_imports VALUES ('frozen', 'hash')")
            db.execute("DROP TABLE targets")
            db.execute("DROP TABLE minute_targets")
            tables = ("hours","apps","minutes","minute_apps","spike_settings","checkpoint","history_imports")
            before = {t:db.execute("SELECT * FROM "+t).fetchall() for t in tables}
        upgraded = Store(self.path)
        with closing(sqlite3.connect(self.path)) as db, db:
            self.assertEqual(before, {t:db.execute("SELECT * FROM "+t).fetchall() for t in tables})
        self.assertEqual(upgraded.app_targets(datetime.now().date(), "old.exe")[0].app,"历史未记录域名")
