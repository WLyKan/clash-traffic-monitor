from datetime import datetime
from pathlib import Path
from tempfile import TemporaryDirectory
import unittest

from clash_monitor.alerts import AlertSettings, MIB, SpikeDetector, WINDOW_SECONDS
from clash_monitor.collector import Collector
from clash_monitor.source import Connection, Snapshot, SourceError
from clash_monitor.storage import Store, minute_bucket


class DetectorTests(unittest.TestCase):
    def test_full_rolling_window_top_apps_peak_and_cooldown(self):
        detector = SpikeDetector(AlertSettings(True, 1))
        t = datetime(2026, 11, 11, 3).timestamp()
        self.assertIsNone(detector.add(t, t + 1, {"Code.exe": (MIB, 0)}))
        for second in range(1, WINDOW_SECONDS - 1):
            self.assertIsNone(detector.add(t + second, t + second + 1, {}))
        alert = detector.add(t + 899, t + 900, {"code.EXE": (0, 50), "other.exe": (0, 100)})
        self.assertEqual((alert.start, alert.end, alert.total), (t, t + 900, MIB + 150))
        self.assertEqual(alert.contributors, ("Code.exe", "other.exe"))
        self.assertEqual(alert.peak_at, minute_bucket(t))
        self.assertIsNone(detector.add(t + 900, t + 901, {"code.EXE": (MIB, 0)}))
        self.assertEqual(detector.total, MIB + 150)  # Old peak is out of the moving window.
        for second in range(901, 1799):
            self.assertIsNone(detector.add(t + second, t + second + 1, {}))
        alert = detector.add(t + 1799, t + 1800, {None: (0, 1)})
        self.assertEqual(alert.contributors, ("code.EXE", None))

    def test_disabled_and_partial_window_do_not_alert(self):
        detector = SpikeDetector(AlertSettings())
        self.assertIsNone(detector.add(0, 900, {"app.exe": (MIB * 1000, 0)}))
        detector.settings = AlertSettings(True, 1)
        self.assertIsNone(detector.add(0, 2, {"app.exe": (MIB, 0)}))
        detector.reset()
        self.assertIsNone(detector.add(1000, 1001, {"app.exe": (MIB, 0)}))
        for second in range(1001, 1899):
            self.assertIsNone(detector.add(second, second + 1, {}))
        self.assertIsNotNone(detector.add(1899, 1900, {}))

    def test_interval_crossing_window_start_is_excluded(self):
        detector = SpikeDetector(AlertSettings(True, 1))
        self.assertIsNone(detector.add(0, 2, {"app.exe": (MIB, 0)}))
        for start in range(2, 899, 3):
            self.assertIsNone(detector.add(start, start + 3, {}))
        self.assertIsNone(detector.add(899, 901, {}))
        self.assertEqual(detector.total, 0)


class SnapshotSource:
    def __init__(self, snapshots):
        self.snapshots = iter(snapshots)

    def sample(self):
        result = next(self.snapshots)
        if isinstance(result, SourceError):
            raise result
        return result


class CollectorAlertTests(unittest.TestCase):
    def test_persisted_settings_and_only_confirmed_proxy_after_full_coverage(self):
        directory = self.enterContext(TemporaryDirectory())
        store = Store(Path(directory) / "traffic.sqlite3")
        config = Collector(None, store)
        self.assertEqual(config.alert_settings(), AlertSettings())
        config.set_alert_settings(AlertSettings(True, 1))
        self.assertEqual(Collector(None, Store(store.path)).alert_settings(), AlertSettings(True, 1))

        t = datetime(2026, 11, 11, 3).timestamp()
        def snapshot(second, proxy=0, generation=1):
            direct = second * MIB
            unknown = second * MIB
            return Snapshot(t + second, 0, proxy + direct + unknown, (
                Connection("direct", t - 10, "direct.exe", "direct", 0, direct),
                Connection("unknown", t - 10, "unknown.exe", "unknown", 0, unknown),
                Connection("proxy", t - 10, "proxy.exe", "proxy", 0, proxy),
            ), generation=generation)

        snapshots = (
            [snapshot(second) for second in range(0, 901, 3)] +
            [snapshot(903, MIB, generation=2), snapshot(906, 2 * MIB, generation=2), SourceError("offline")] +
            [snapshot(second, 3 * MIB, generation=2) for second in range(909, 1809, 3)] +
            [snapshot(1809, 4 * MIB, generation=2), snapshot(1812, 5 * MIB, generation=2)]
        )
        collector = Collector(SnapshotSource(snapshots), store)
        for _ in snapshots[:-2]:
            collector.poll()
            self.assertIsNone(collector.take_alert())
        collector.poll()
        alert = collector.take_alert()
        self.assertEqual((alert.start, alert.end, alert.total, alert.contributors),
                         (t + 909, t + 1809, MIB, ("proxy.exe",)))
        collector.poll()
        self.assertIsNone(collector.take_alert())
        collector.set_alert_settings(AlertSettings(False, 2))
        self.assertEqual(Collector(None, Store(store.path)).alert_settings(), AlertSettings(False, 2))
