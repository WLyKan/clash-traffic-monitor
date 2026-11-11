from contextlib import closing
from datetime import datetime
from pathlib import Path
import sqlite3
import subprocess
import sys
from tempfile import TemporaryDirectory
import unittest
from unittest.mock import patch

from clash_monitor.recovery import HistorySource, RecoveryError, merge_history
from clash_monitor.storage import Store, minute_bucket


class RecoveryTests(unittest.TestCase):
    def setUp(self):
        self.directory = Path(self.enterContext(TemporaryDirectory()))
        self.target = self.directory / "target" / "traffic.sqlite3"
        self.sampled_at = datetime(2026, 11, 11, 14, 14).timestamp()
        self.bucket = minute_bucket(self.sampled_at)

    def current_source(self, name, *, app="Code.exe", uploaded=10, downloaded=20,
                       sample_offset=0, settings=(0, 100), segment=1):
        path = self.directory / f"{name}-snapshot.sqlite3"
        Store(path)
        with closing(sqlite3.connect(path)) as db, db:
            db.execute("""
                INSERT INTO hours (day, hour, samples, gap_seconds, covered_seconds,
                    unattributed_up, unattributed_down, unmatched_up, unmatched_down, legacy_coverage)
                VALUES ('2026-11-11', 14, 3, 2.5, 4.5, 5, 6, -7, 8, 0)
            """)
            db.execute("INSERT INTO apps VALUES ('2026-11-11', 14, ?, ?, ?)", (app, uploaded, downloaded))
            db.execute("INSERT INTO minutes VALUES (?, 3, 4.5, 2.5, 5, 6)", (self.bucket,))
            db.execute("INSERT INTO minute_apps VALUES (?, ?, ?, ?)", (self.bucket, app, uploaded, downloaded))
            db.execute("INSERT INTO minute_residuals VALUES (?, ?, -7, 8)", (self.bucket, segment))
            db.execute("INSERT INTO checkpoint VALUES (1, ?)", (self.sampled_at + sample_offset,))
            if settings is not None:
                db.execute("INSERT INTO spike_settings VALUES (1, ?, ?)", settings)
        return HistorySource(path, str(self.directory / f"{name}-original.sqlite3"))

    def rows(self, table, path=None):
        with closing(sqlite3.connect(path or self.target)) as db:
            return db.execute(f"SELECT * FROM {table} ORDER BY 1, 2").fetchall()

    def database_dump(self, path=None):
        with closing(sqlite3.connect(path or self.target)) as db:
            return tuple(db.iterdump())

    def test_adds_conflicting_hour_minute_and_case_insensitive_apps(self):
        newer = self.current_source("newer", settings=(1, 200), sample_offset=300)
        older = self.current_source("older", app="code.EXE", uploaded=30, downloaded=40,
                                    settings=(0, 50), segment=2)
        results = merge_history(self.target, [newer, older])
        self.assertEqual([result.imported for result in results], [True, True])
        hours = Store(self.target).day_hours(datetime(2026, 11, 11).date())[14]
        self.assertEqual((hours.uploaded, hours.downloaded, hours.samples), (40, 60, 6))
        self.assertEqual((hours.covered_seconds, hours.gap_seconds), (9, 5))
        self.assertEqual(self.rows("apps"), [("2026-11-11", 14, "Code.exe", 40, 60)])
        self.assertEqual(self.rows("minutes"), [(self.bucket, 6, 9, 5, 10, 12)])
        self.assertEqual(self.rows("minute_apps"), [(self.bucket, "Code.exe", 40, 60)])
        self.assertEqual(self.rows("minute_residuals"), [
            (self.bucket, 1, -7, 8), (self.bucket, 2, -7, 8),
        ])
        self.assertEqual(self.rows("checkpoint"), [(1, self.sampled_at + 300)])
        self.assertEqual(self.rows("spike_settings"), [(1, 1, 200)])
        with closing(sqlite3.connect(self.target)) as db:
            self.assertEqual(db.execute("PRAGMA quick_check").fetchall(), [("ok",)])
            self.assertEqual(db.execute("PRAGMA foreign_key_check").fetchall(), [])

    def test_same_residual_segment_is_added_without_reconciling_again(self):
        first = self.current_source("first")
        second = self.current_source("second")
        merge_history(self.target, [first, second])
        self.assertEqual(self.rows("minute_residuals"), [(self.bucket, 1, -14, 16)])
        with closing(sqlite3.connect(self.target)) as db:
            self.assertEqual(db.execute("SELECT unmatched_up, unmatched_down FROM hours").fetchone(), (-14, 16))

    def test_repeat_snapshot_is_a_noop_even_with_a_different_snapshot_filename(self):
        source = self.current_source("source")
        merge_history(self.target, [source])
        before = self.database_dump()
        copy = self.directory / "copied-snapshot.sqlite3"
        with closing(sqlite3.connect(source.snapshot)) as original, closing(sqlite3.connect(copy)) as backup:
            original.backup(backup)
        repeated = merge_history(self.target, [HistorySource(copy, source.source_key)])
        self.assertFalse(repeated[0].imported)
        self.assertEqual(self.database_dump(), before)

    def test_provenance_paths_are_not_resolved_through_appdata_virtualization(self):
        ordinary = self.current_source("ordinary")
        virtualized = self.current_source("virtualized", uploaded=30)
        original_resolve = Path.resolve

        def redirect_originals(path, *args, **kwargs):
            if str(path) in (ordinary.source_key, virtualized.source_key):
                return Path(virtualized.source_key)
            return original_resolve(path, *args, **kwargs)

        with patch.object(Path, "resolve", redirect_originals):
            results = merge_history(self.target, [ordinary, virtualized])
        self.assertEqual([row.imported for row in results], [True, True])
        self.assertEqual(len(self.rows("history_imports")), 2)
        self.assertEqual(self.rows("apps")[0][-2:], (40, 40))

    def test_changed_snapshot_rejects_and_rolls_back_earlier_source_in_same_call(self):
        existing = self.current_source("existing")
        merge_history(self.target, [existing])
        before = self.database_dump()
        extra = self.current_source("extra", uploaded=99)
        with closing(sqlite3.connect(existing.snapshot)) as db, db:
            db.execute("UPDATE apps SET uploaded = uploaded + 1")
        with self.assertRaisesRegex(RecoveryError, "不同内容"):
            merge_history(self.target, [extra, existing])
        self.assertEqual(self.database_dump(), before)

    def test_sql_failure_rolls_back_all_statistics_and_import_ledger(self):
        source = self.current_source("source")
        Store(self.target)
        with closing(sqlite3.connect(self.target)) as db, db:
            db.execute("""
                CREATE TRIGGER reject_minutes BEFORE INSERT ON minute_apps
                BEGIN SELECT RAISE(ABORT, 'test import interruption'); END
            """)
        before = self.database_dump()
        with self.assertRaises(sqlite3.IntegrityError):
            merge_history(self.target, [source])
        self.assertEqual(self.database_dump(), before)
        with closing(sqlite3.connect(self.target)) as db, db:
            db.execute("DROP TRIGGER reject_minutes")
        self.assertTrue(merge_history(self.target, [source])[0].imported)

    def test_old_schema_is_read_only_and_retains_unknown_coverage(self):
        path = self.directory / "legacy-snapshot.sqlite3"
        with closing(sqlite3.connect(path)) as db, db:
            db.executescript("""
                CREATE TABLE hours (day TEXT, hour INTEGER, samples INTEGER, gap_seconds REAL,
                    unattributed_up INTEGER, unattributed_down INTEGER, unmatched_up INTEGER,
                    unmatched_down INTEGER, PRIMARY KEY(day, hour));
                CREATE TABLE apps (day TEXT, hour INTEGER, app TEXT COLLATE NOCASE,
                    uploaded INTEGER, downloaded INTEGER, PRIMARY KEY(day, hour, app));
                CREATE TABLE checkpoint (id INTEGER PRIMARY KEY, sampled_at REAL);
                INSERT INTO hours VALUES ('2026-11-11', 14, 8, 2, 3, 4, -5, 6);
                INSERT INTO apps VALUES ('2026-11-11', 14, 'legacy.exe', 10, 12);
                INSERT INTO checkpoint VALUES (1, 1);
            """)
        before = self.database_dump(path)
        merge_history(self.target, [HistorySource(path, str(self.directory / "legacy-original.sqlite3"))])
        row = Store(self.target).day_hours(datetime(2026, 11, 11).date())[14]
        self.assertEqual((row.uploaded, row.downloaded, row.samples), (10, 12, 8))
        self.assertTrue(row.legacy_coverage)
        self.assertEqual(row.covered_seconds, 0)
        self.assertEqual(self.rows("minutes"), [])
        self.assertEqual(self.database_dump(path), before)

    def test_existing_target_settings_and_newer_checkpoint_are_preserved(self):
        source = self.current_source("source", settings=(1, 20))
        Store(self.target)
        with closing(sqlite3.connect(self.target)) as db, db:
            db.execute("INSERT INTO spike_settings VALUES (1, 0, 800)")
            db.execute("INSERT INTO checkpoint VALUES (1, ?)", (self.sampled_at + 1000,))
        merge_history(self.target, [source])
        self.assertEqual(self.rows("spike_settings"), [(1, 0, 800)])
        self.assertEqual(self.rows("checkpoint"), [(1, self.sampled_at + 1000)])

    def test_rejects_live_source_or_target_as_source(self):
        source = self.current_source("source")
        with self.assertRaisesRegex(RecoveryError, "冻结"):
            merge_history(self.target, [HistorySource(source.snapshot, str(source.snapshot))])
        with self.assertRaisesRegex(RecoveryError, "目标"):
            merge_history(source.snapshot, [source])
        self.assertFalse(self.target.exists())

    def test_rejects_target_own_backup_as_an_additive_source(self):
        Store(self.target)
        before = self.database_dump()
        snapshot = self.directory / "target-backup.sqlite3"
        with closing(sqlite3.connect(self.target)) as original, closing(sqlite3.connect(snapshot)) as backup:
            original.backup(backup)
        source = HistorySource(snapshot, str(self.target.resolve()))
        with self.assertRaisesRegex(RecoveryError, "自身"):
            merge_history(self.target, [source])
        self.assertEqual(self.database_dump(), before)

    def test_cli_refuses_target_directory_with_an_active_writer_lock(self):
        import msvcrt

        source = self.current_source("source")
        self.target.parent.mkdir()
        with (self.target.parent / "collector.lock").open("a+b") as lock:
            msvcrt.locking(lock.fileno(), msvcrt.LK_NBLCK, 1)
            result = subprocess.run([
                sys.executable, "-B", "tools/merge_history.py", "--target", str(self.target),
                "--source", str(source.snapshot), "--source-key", source.source_key,
            ], text=True, capture_output=True, encoding="utf-8", errors="replace", timeout=10,
               creationflags=subprocess.CREATE_NO_WINDOW)
        self.assertEqual(result.returncode, 1)
        self.assertFalse(self.target.exists())


if __name__ == "__main__":
    unittest.main()
