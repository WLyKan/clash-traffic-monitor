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

    def test_week_and_month_reset_independently_at_local_calendar_boundaries(self):
        friday = date(2026, 5, 1)
        sunday = date(2026, 5, 3)
        monday = date(2026, 5, 4)
        self.record(friday - timedelta(days=1), "april.exe", 0, 8, 23)
        self.record(friday, "friday.exe", 0, 4)
        self.record(sunday, "sunday.exe", 0, 5, 23)
        self.record(monday, "monday.exe", 0, 3)
        self.assertEqual(trend_start(3, sunday), date(2026, 4, 27))
        _, week = self.store.range_traffic(trend_start(3, sunday), sunday)
        self.assertEqual([row.app for row in week], ["april.exe", "sunday.exe", "friday.exe"])
        _, week = self.store.range_traffic(trend_start(3, monday), monday)
        self.assertEqual(week, [AppTraffic("monday.exe", 0, 3)])
        self.assertEqual(trend_start(4, monday), friday)
        _, month = self.store.range_traffic(trend_start(4, monday), monday)
        self.assertEqual([row.app for row in month], ["sunday.exe", "friday.exe", "monday.exe"])
        self.assertEqual(trend_start(4, date(2026, 6, 1)), date(2026, 6, 1))
