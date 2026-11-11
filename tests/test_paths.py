from contextlib import closing, nullcontext
from io import StringIO
import json
import os
from pathlib import Path
import sqlite3
import sys
from tempfile import TemporaryDirectory
from types import SimpleNamespace
import unittest
from unittest.mock import Mock, patch

from clash_monitor import __main__ as entry, paths, startup


class DataDirectoryTests(unittest.TestCase):
    def test_default_uses_profile_documents_and_ignores_appdata(self):
        profile = Path(r"C:\Users\Example")
        with patch.dict(os.environ, {
            "USERPROFILE": str(profile), "LOCALAPPDATA": r"D:\Package\Local",
            "APPDATA": r"D:\Package\Roaming",
        }):
            expected = (profile / "Documents" / "ClashTrafficMonitor").resolve()
            self.assertEqual(paths.resolve_data_dir(), expected)
            with patch.dict(os.environ, {"LOCALAPPDATA": r"C:\Users\Example\AppData\Local"}):
                self.assertEqual(paths.resolve_data_dir(), expected)

    def test_missing_profile_falls_back_to_home_and_relative_profile_is_rejected(self):
        with patch.dict(os.environ, {"USERPROFILE": ""}), patch.object(
            paths.Path, "home", return_value=Path(r"C:\Users\Fallback")
        ):
            self.assertEqual(paths.default_data_dir(), Path(r"C:\Users\Fallback\Documents\ClashTrafficMonitor"))
        with patch.dict(os.environ, {"USERPROFILE": "relative-profile"}):
            with self.assertRaisesRegex(OSError, "绝对路径"):
                paths.default_data_dir()

    def test_custom_path_does_not_require_user_profile(self):
        with patch.dict(os.environ, {"USERPROFILE": "relative-profile"}):
            self.assertEqual(paths.resolve_data_dir(Path(".")), Path.cwd().resolve())

    def test_explicit_default_retains_startup_control_and_custom_isolated_dir_does_not(self):
        default_dir = paths.default_data_dir()
        for directory, allowed in ((default_dir, True), (default_dir / "isolated-test", False)):
            with self.subTest(directory=directory), patch.object(
                sys, "argv", ["clash-monitor", "--data-dir", str(directory)]
            ), patch.object(entry, "MihomoSource"), patch.object(
                entry, "single_writer", return_value=nullcontext()
            ), patch.object(entry, "Store"), patch.object(entry, "Collector"), patch.object(
                startup, "LoginStartup"
            ) as login, patch.dict(sys.modules, {
                "clash_monitor.tray": SimpleNamespace(run_dashboard=Mock())
            }):
                self.assertEqual(entry.main(), 0)
                if allowed:
                    login.assert_called_once_with(default_dir)
                else:
                    login.assert_not_called()


class DiagnosticsTests(unittest.TestCase):
    def test_missing_directory_diagnostics_never_start_services_or_create_files(self):
        with TemporaryDirectory() as temporary:
            directory = Path(temporary).resolve() / "absent"
            output = StringIO()
            with patch.object(sys, "argv", [
                "clash-monitor", "--data-dir", str(directory), "--diagnostics"
            ]), patch.object(sys, "stdout", output), patch.object(
                entry, "MihomoSource", side_effect=AssertionError("must not connect")
            ), patch.object(entry, "Store", side_effect=AssertionError("must not create DB")), patch.object(
                entry, "single_writer", side_effect=AssertionError("must not lock")
            ), patch.object(startup.winreg, "SetValueEx", side_effect=AssertionError("must not set startup")):
                self.assertEqual(entry.main(), 0)
            report = json.loads(output.getvalue())
            self.assertEqual(report["effective_data_dir"], str(directory))
            self.assertFalse(report["database_exists"])
            self.assertIsNone(report["checkpoint"])
            self.assertFalse(directory.exists())

    def test_immutable_diagnostics_read_old_schema_without_touching_database_or_wal(self):
        with TemporaryDirectory() as temporary:
            directory = Path(temporary)
            database = directory / "traffic.sqlite3"
            with closing(sqlite3.connect(database)) as writer:
                writer.executescript("""
                    PRAGMA journal_mode = WAL;
                    PRAGMA wal_autocheckpoint = 0;
                    CREATE TABLE hours (day TEXT, hour INTEGER, samples INTEGER, gap_seconds REAL);
                    CREATE TABLE apps (day TEXT, hour INTEGER, app TEXT, uploaded INTEGER, downloaded INTEGER);
                    CREATE TABLE checkpoint (id INTEGER PRIMARY KEY, sampled_at REAL);
                    INSERT INTO hours VALUES ('2026-11-10', 2, 12, 3);
                    INSERT INTO apps VALUES ('2026-11-10', 2, 'sample.exe', 200, 800);
                    INSERT INTO checkpoint VALUES (1, 1794247200);
                """)
                writer.execute("PRAGMA wal_checkpoint(TRUNCATE)")
                writer.execute("UPDATE apps SET downloaded = 999")
                writer.commit()
                files = [database, database.with_name(database.name + "-wal"), database.with_name(database.name + "-shm")]
                before = {file.name: (file.read_bytes(), file.stat().st_mtime_ns) for file in files}
                report = entry.diagnostics(directory)
                after = {file.name: (file.read_bytes(), file.stat().st_mtime_ns) for file in files}
                self.assertEqual(before, after)
                self.assertTrue(report["wal_exists"])
                self.assertGreater(report["wal_bytes"], 0)
                self.assertEqual(report["snapshot_mode"], "immutable")
                self.assertEqual(report["days"], [{
                    "day": "2026-11-10", "samples": 12, "gap_seconds": 3,
                    "covered_seconds": None, "uploaded": 200, "downloaded": 800,
                }])
                self.assertEqual(report["checkpoint"]["sampled_at"], 1794247200)
                self.assertNotIn("database_error", report)


if __name__ == "__main__":
    unittest.main()
