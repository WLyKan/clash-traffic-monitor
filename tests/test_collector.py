from contextlib import closing
from datetime import datetime, timedelta
from pathlib import Path
from tempfile import TemporaryDirectory
import sqlite3
import unittest

from clash_monitor.collector import Collector, traffic_delta
from clash_monitor.source import Connection, Snapshot, SourceError
from clash_monitor.storage import AppTraffic, Store


class ScriptedSource:
    def __init__(self, *results):
        self.results = iter(results)

    def sample(self):
        result = next(self.results)
        if isinstance(result, SourceError):
            raise result
        return result


def connection(id, route, app, uploaded, downloaded, started_at=0):
    return Connection(id, started_at, app, route, uploaded, downloaded)


class CollectorTests(unittest.TestCase):
    def setUp(self):
        directory = self.enterContext(TemporaryDirectory())
        self.store = Store(Path(directory) / "traffic.sqlite3")

    def test_delta_categorizes_proxy_and_unknown_route_without_counting_direct(self):
        before = Snapshot(100, 1000, 2000, (
            connection("old", "proxy", "Code.exe", 20, 30),
            connection("direct", "direct", "chrome.exe", 0, 0),
        ))
        after = Snapshot(101, 1090, 2290, (
            connection("old", "proxy", "Code.exe", 40, 50),
            connection("direct", "direct", "chrome.exe", 30, 200),
            connection("unknown-route", "unknown", "Code.exe", 10, 10, 100.2),
            connection("unknown-app", "proxy", None, 5, 5, 100.4),
            connection("missed-baseline", "proxy", "other.exe", 500, 800),
        ))
        apps, unknown_route, unmatched = traffic_delta(before, after)
        self.assertEqual(apps, {"Code.exe": (20, 20), None: (5, 5)})
        self.assertEqual(unknown_route, (10, 10))
        self.assertEqual(unmatched, (25, 55))

    def test_snapshot_race_reconciles_signed_residuals_within_hour(self):
        hour = datetime(2026, 11, 10, 3)
        start = hour.timestamp()
        collector = Collector(ScriptedSource(
            Snapshot(start, 100, 100, (connection("stream", "proxy", "app.exe", 100, 100),)),
            Snapshot(start + 1, 100, 200, (connection("stream", "proxy", "app.exe", 200, 100),)),
            Snapshot(start + 2, 200, 200, (connection("stream", "proxy", "app.exe", 200, 200),)),
        ), self.store)
        for _ in range(3):
            collector.poll()
        row = self.store.day_hours(hour.date())[3]
        self.assertEqual((row.uploaded, row.downloaded), (100, 100))
        self.assertEqual((row.unattributed_up, row.unattributed_down), (0, 0))

    def test_first_seen_connection_started_during_previous_request_sets_baseline(self):
        before = Snapshot(100, 10, 10, (), completed_at=100.1)
        after = Snapshot(101, 15, 15, (connection("new", "proxy", "app.exe", 12, 12, 100.05),))
        self.assertEqual(traffic_delta(before, after), ({}, (0, 0), (5, 5)))

    def test_rollover_stores_only_deltas_and_joins_case_variants(self):
        midnight = datetime(2026, 11, 11)
        boundary = midnight.timestamp()
        collector = Collector(ScriptedSource(
            Snapshot(boundary - 2, 40, 50, (connection("stream", "proxy", "Code.exe", 40, 50),)),
            Snapshot(boundary - 1, 50, 70, (connection("stream", "proxy", "Code.exe", 50, 70),)),
            Snapshot(boundary, 60, 90, (connection("stream", "proxy", "Code.exe", 60, 90),)),
            Snapshot(boundary + 1, 65, 99, (connection("stream", "proxy", "code.EXE", 65, 99),)),
        ), self.store)
        for _ in range(4):
            collector.poll()
        prior = datetime.fromtimestamp(boundary - 1).date()
        self.assertEqual(self.store.hour_apps(prior, 23), [AppTraffic("Code.exe", 10, 20)])
        self.assertEqual(self.store.hour_apps(midnight.date(), 0), [AppTraffic("Code.exe", 15, 29)])
        hours = self.store.day_hours(midnight.date())
        self.assertEqual(len(hours), 24)
        self.assertEqual((hours[0].uploaded, hours[0].downloaded, hours[0].samples), (15, 29, 2))
        self.assertEqual(hours[1].samples, 0)

    def test_offline_restart_and_core_reset_create_gaps_without_backfill(self):
        hour = datetime(2026, 11, 10, 12)
        boundary = hour.timestamp()
        collector = Collector(ScriptedSource(
            Snapshot(boundary - 1, 9, 10, ()),
            SourceError("离线"),
            Snapshot(boundary + 3, 99, 100, ()),
            Snapshot(boundary + 4, 1, 2, ()),
        ), self.store)
        collector.poll()
        collector.poll()
        self.assertFalse(collector.status().connected)
        self.assertEqual(collector.status().last_sample_at, boundary - 1)
        collector.poll()
        collector.poll()
        self.assertTrue(collector.status().connected)
        restarted = Collector(ScriptedSource(Snapshot(boundary + 9, 20, 30, ())), self.store)
        restarted.poll()
        hours = self.store.day_hours(hour.date())
        self.assertEqual((hours[11].gap_seconds, hours[12].gap_seconds), (1, 9))
        self.assertEqual((hours[12].unattributed_up, hours[12].unattributed_down), (0, 0))
        self.assertEqual(self.store.last_sample(), boundary + 9)

    def test_ranking_uses_combined_volume_and_keeps_unknown_app(self):
        hour = datetime(2026, 11, 10, 5)
        self.store.record(hour.timestamp(), {"slow.exe": (5, 8), None: (15, 1), "fast.exe": (4, 30)}, (1, 4))
        self.assertEqual([row.app for row in self.store.hour_apps(hour.date(), 5)], ["fast.exe", None, "slow.exe"])
        day = self.store.day_hours(hour.date())
        self.assertEqual((day[5].uploaded, day[5].downloaded), (24, 39))
        self.assertEqual((day[5].unattributed_up, day[5].unattributed_down), (1, 4))

    def test_coverage_splits_at_midnight_and_excludes_sleep_and_offline_time(self):
        midnight = datetime(2026, 11, 11)
        t = midnight.timestamp()
        day = midnight.date()
        prior = datetime.fromtimestamp(t - 1).date()
        collector = Collector(ScriptedSource(
            Snapshot(t - 1, 0, 0, ()),
            Snapshot(t + 1, 0, 0, ()),
            Snapshot(t + 61, 0, 0, ()),
            SourceError("offline"),
            Snapshot(t + 65, 0, 0, ()),
            Snapshot(t + 66, 0, 0, ()),
        ), self.store)
        collector.poll()
        collector.poll()
        self.assertEqual(self.store.day_hours(prior)[23].covered_seconds, 1)
        self.assertEqual(self.store.day_hours(day, as_of=t + 3)[0].gap_seconds, 0)
        collector.poll()
        collector.poll()
        self.assertEqual(self.store.day_hours(day, as_of=t + 63, gap_tolerance=0)[0].gap_seconds, 62)
        for _ in range(2):
            collector.poll()
        row = self.store.day_hours(day, as_of=t + 67)[0]
        self.assertEqual((row.covered_seconds, row.gap_seconds, row.downloaded), (2, 64, 0))

    def test_old_database_keeps_unknown_coverage_after_new_samples_in_same_hour(self):
        # Remove the additive columns to reproduce the last released schema.
        hour = datetime(2026, 11, 10, 5)
        t = hour.timestamp()
        self.store.record(t, {"legacy.exe": (12, 34)}, (0, 0))
        with closing(sqlite3.connect(self.store.path)) as db, db:
            db.execute("ALTER TABLE hours DROP COLUMN legacy_coverage")
            db.execute("ALTER TABLE hours DROP COLUMN covered_seconds")
        upgraded = Store(self.store.path)
        upgraded.record(t + 2, {}, (0, 0), covered_from=t + 1)
        row = Store(self.store.path).day_hours(hour.date())[5]
        self.assertTrue(row.legacy_coverage)
        self.assertEqual((row.covered_seconds, row.uploaded, row.downloaded), (1, 12, 34))

    def test_adjacent_five_minute_buckets_reconcile_both_signed_directions(self):
        for day, hour, minute in ((11, 2, 5), (11, 3, 0), (12, 0, 0)):
            with self.subTest(hour=hour, minute=minute):
                store = Store(self.store.path.with_name(f"boundary-{day}-{hour}-{minute}.sqlite3"))
                edge = datetime(2026, 11, day, hour, minute)
                t = edge.timestamp()
                previous = edge - timedelta(minutes=5)
                collector = Collector(ScriptedSource(
                    Snapshot(t - 2, 100, 100, (connection("proxy", "proxy", "app.exe", 100, 100),)),
                    Snapshot(t - 1, 110, 100, (
                        connection("proxy", "proxy", "app.exe", 100, 110),
                        connection("direct", "direct", "direct.exe", 0, 20, t - 1.5),
                    )),
                    Snapshot(t, 110, 100, (
                        connection("proxy", "proxy", "app.exe", 100, 110),
                        connection("direct", "direct", "direct.exe", 0, 20, t - 1.5),
                    )),
                    Snapshot(t + 1, 115, 135, (
                        connection("proxy", "proxy", "app.exe", 110, 110),
                        connection("direct", "direct", "direct.exe", 0, 20, t - 1.5),
                        connection("unknown", "unknown", "unknown.exe", 2, 0, t + 0.5),
                    )),
                ), store)
                for _ in range(4):
                    collector.poll()
                prior, prior_apps = store.minute_detail(previous.date(), previous.hour, previous.minute)
                current, current_apps = store.minute_detail(edge.date(), hour, minute)
                self.assertEqual((prior.unattributed_up, prior.unattributed_down), (3, 0))
                self.assertEqual((current.unattributed_up, current.unattributed_down), (2, 5))
                self.assertEqual((prior.covered_seconds, current.covered_seconds), (2, 1))
                self.assertEqual(prior_apps, [AppTraffic("app.exe", 0, 10)])
                self.assertEqual(current_apps, [AppTraffic("app.exe", 10, 0)])
                if previous.hour != hour:
                    self.assertEqual(store.day_hours(previous.date())[previous.hour].unattributed_up, 3)
                    self.assertEqual(store.day_hours(edge.date())[hour].unattributed_down, 5)

    def test_minute_residuals_do_not_offset_across_reconnection(self):
        edge = datetime(2026, 11, 11, 2, 5)
        t = edge.timestamp()
        self.store.record(t - 1, {}, (0, 0), unmatched=(10, 0), covered_from=t - 2, segment=t - 2)
        self.store.record(t, {}, (0, 0), gap_from=t - 1)
        self.store.record(t + 1, {"proxy.exe": (10, 0)}, (0, 0), unmatched=(-10, 0), covered_from=t, segment=t)
        before, _ = self.store.minute_detail(edge.date(), 2, 0)
        after, _ = self.store.minute_detail(edge.date(), 2, 5)
        self.assertEqual((before.unattributed_up, before.gap_seconds, after.unattributed_up), (10, 1, 0))

    def test_detail_migration_does_not_backfill_and_retention_preserves_hours(self):
        start = datetime(2026, 11, 1, 5)
        t = start.timestamp()
        self.store.record(t, {"legacy.exe": (10, 20)}, (0, 0))
        with closing(sqlite3.connect(self.store.path)) as db, db:
            for table in ("minute_apps", "minute_residuals", "minutes"):
                db.execute(f"DROP TABLE {table}")
        self.store = Store(self.store.path)
        self.store.record(t + 360, {}, (0, 0), gap_from=t)
        legacy, legacy_apps = self.store.minute_detail(start.date(), 5, 0)
        self.assertEqual((legacy.samples, legacy.gap_seconds, legacy_apps), (0, 0, []))
        self.store.record(t + 361, {"new.exe": (2, 3)}, (0, 0), covered_from=t + 360)
        row, apps = self.store.minute_detail(start.date(), 5, 5)
        self.assertEqual((row.covered_seconds, apps), (1, [AppTraffic("new.exe", 2, 3)]))
        self.store.record(t + 14 * 86400 + 600, {}, (0, 0))
        self.assertEqual(self.store.minute_detail(start.date(), 5, 5)[1], [])
        self.assertEqual(self.store.hour_apps(start.date(), 5), [
            AppTraffic("legacy.exe", 10, 20), AppTraffic("new.exe", 2, 3),
        ])

    def test_core_generation_change_does_not_count_old_or_new_baselines(self):
        day = datetime(2026, 11, 10, 8)
        start = day.timestamp()
        collector = Collector(ScriptedSource(
            Snapshot(start, 10, 20, (), generation=1),
            Snapshot(start + 1, 200, 500, (), generation=2),
        ), self.store)
        collector.poll()
        collector.poll()
        hour = self.store.day_hours(day.date())[8]
        self.assertEqual((hour.samples, hour.gap_seconds, hour.unattributed_up, hour.unattributed_down), (2, 1, 0, 0))
        self.assertEqual(hour.covered_seconds, 0)


if __name__ == "__main__":
    unittest.main()
