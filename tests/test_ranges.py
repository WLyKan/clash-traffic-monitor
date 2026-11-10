from datetime import date, datetime, time, timedelta
from pathlib import Path
from tempfile import TemporaryDirectory
import unittest

from clash_monitor.storage import AppTraffic, Store
from clash_monitor.ui import trend_start


class RangeTests(unittest.TestCase):
    def setUp(self):
        directory = self.enterContext(TemporaryDirectory())
        self.store = Store(Path(directory) / "traffic.sqlite3")

    def record(self, day, app, up, down, hour=0):
        timestamp = datetime.combine(day, time(hour, 5)).timestamp()
        self.store.record(timestamp, {app: (up, down)}, (0, 0), covered_from=timestamp - 1)

    def test_recent_ranges_include_today_and_reuse_hourly_records(self):
        today = date(2026, 1, 3)
        start = trend_start(1, today)
        self.assertEqual(start, date(2025, 12, 28))
        self.assertEqual(trend_start(2, today), date(2025, 12, 5))
        self.record(start - timedelta(days=1), "outside.exe", 100, 100)
        self.record(start, "Code.exe", 2, 4)
        self.record(today, "code.EXE", 5, 10, 1)
        self.record(today, None, 0, 1, 2)
        self.record(today + timedelta(days=1), "future.exe", 100, 100)
        days, apps = self.store.range_traffic(start, today)
        self.assertEqual(len(days), 7)
        self.assertEqual((days[0].day, days[-1].day), (start, today))
        self.assertEqual(days[1].samples, 0)
        self.assertEqual((days[-1].uploaded, days[-1].downloaded, days[-1].covered_seconds), (5, 11, 2))
        self.assertEqual((apps[0].app.lower(), apps[0].uploaded, apps[0].downloaded), ("code.exe", 7, 14))
        self.assertEqual(apps[1:], [AppTraffic(None, 0, 1)])
        month_days, month_apps = self.store.range_traffic(trend_start(2, today), today)
        self.assertEqual(len(month_days), 30)
        self.assertEqual(month_apps[0].app, "outside.exe")
