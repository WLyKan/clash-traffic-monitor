import argparse
from contextlib import closing, contextmanager
import ctypes
from ctypes import wintypes
from datetime import datetime
import json
import msvcrt
import os
from pathlib import Path
import sqlite3
import sys

from clash_monitor.collector import Collector
from clash_monitor.paths import default_data_dir, resolve_data_dir
from clash_monitor.source import MihomoSource, SourceError
from clash_monitor.storage import Store


class AlreadyRunning(Exception):
    pass


@contextmanager
def single_writer(directory: Path):
    directory.mkdir(parents=True, exist_ok=True)
    with (directory / "collector.lock").open("a+b") as lock:
        lock.seek(0)
        try:
            msvcrt.locking(lock.fileno(), msvcrt.LK_NBLCK, 1)
        except OSError as error:
            raise AlreadyRunning from error
        yield


def show_message(message: str, *, error: bool = False) -> None:
    stream = sys.stderr if error else sys.stdout
    if stream is not None:
        print(message, file=stream)
    else:
        icon = 0x10 if error else 0x40
        ctypes.windll.user32.MessageBoxW(None, message, "Clash 流量监控", icon)


def show_diagnostics(report: dict) -> None:
    message = json.dumps(report, ensure_ascii=False, indent=2)
    if sys.stdout is not None:
        binary = getattr(sys.stdout, "buffer", None)
        if binary is not None:
            binary.write((message + "\n").encode("utf-8"))
            binary.flush()
        else:
            print(message)
        return
    # A windowed executable can still inherit a caller-owned output pipe.
    kernel = ctypes.WinDLL("kernel32", use_last_error=True)
    get_output = kernel.GetStdHandle
    get_output.argtypes = [wintypes.DWORD]
    get_output.restype = wintypes.HANDLE
    write_output = kernel.WriteFile
    write_output.argtypes = [
        wintypes.HANDLE, wintypes.LPCVOID, wintypes.DWORD,
        ctypes.POINTER(wintypes.DWORD), wintypes.LPVOID,
    ]
    write_output.restype = wintypes.BOOL
    handle = get_output(-11 & 0xFFFFFFFF)
    if handle not in (None, wintypes.HANDLE(-1).value):
        remaining = (message + "\n").encode("utf-8")
        while remaining:
            written = wintypes.DWORD()
            if not write_output(handle, remaining, len(remaining), ctypes.byref(written), None) or not written.value:
                break
            remaining = remaining[written.value:]
        else:
            return
    show_message(message)


def diagnostics(data_dir: Path) -> dict:
    from clash_monitor.startup import startup_command

    database = data_dir / "traffic.sqlite3"
    wal = database.with_name(database.name + "-wal")
    report = {
        "effective_data_dir": str(data_dir),
        "physical_data_dir": str(data_dir.resolve()),
        "effective_database_path": str(database),
        "physical_database_path": str(database.resolve()),
        "database_exists": database.is_file(),
        "wal_exists": wal.is_file(),
        "wal_bytes": wal.stat().st_size if wal.is_file() else 0,
        "snapshot_mode": "immutable",
        "snapshot_note": "只读主数据库快照；尚未合入主数据库的 WAL 提交不包含在以下汇总中。",
        "days": [],
        "checkpoint": None,
        "login_startup_command": startup_command(data_dir),
    }
    if not report["database_exists"]:
        return report
    try:
        with closing(sqlite3.connect(database.as_uri() + "?mode=ro&immutable=1", uri=True)) as db:
            tables = {row[0] for row in db.execute("SELECT name FROM sqlite_master WHERE type = 'table'")}
            days = {}
            if "hours" in tables:
                columns = {row[1] for row in db.execute("PRAGMA table_info(hours)")}
                coverage = "SUM(covered_seconds)" if "covered_seconds" in columns else "NULL"
                for day, samples, gaps, covered in db.execute(
                    f"SELECT day, SUM(samples), SUM(gap_seconds), {coverage} FROM hours GROUP BY day"
                ):
                    days[day] = {
                        "day": day, "samples": samples, "gap_seconds": gaps,
                        "covered_seconds": covered, "uploaded": 0, "downloaded": 0,
                    }
            if "apps" in tables:
                for day, uploaded, downloaded in db.execute(
                    "SELECT day, SUM(uploaded), SUM(downloaded) FROM apps GROUP BY day"
                ):
                    entry = days.setdefault(day, {
                        "day": day, "samples": 0, "gap_seconds": 0, "covered_seconds": None,
                    })
                    entry.update(uploaded=uploaded, downloaded=downloaded)
            report["days"] = [days[day] for day in sorted(days)]
            if "checkpoint" in tables:
                row = db.execute("SELECT sampled_at FROM checkpoint WHERE id = 1").fetchone()
                if row:
                    report["checkpoint"] = {
                        "sampled_at": row[0],
                        "local_time": datetime.fromtimestamp(row[0]).astimezone().isoformat(),
                    }
    except (sqlite3.Error, OSError, ValueError, OverflowError):
        report["database_error"] = "无法读取数据库快照，请检查文件完整性和读取权限。"
    return report


def main() -> int:
    parser = argparse.ArgumentParser(description="Clash 流量监控")
    parser.add_argument("--data-dir", type=Path)
    parser.add_argument("--start-hidden", action="store_true", help="启动后只显示通知区域图标")
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument("--check", action="store_true", help="只读检查 Mihomo 连接和应用归因")
    mode.add_argument("--diagnostics", action="store_true", help="只读输出数据目录、历史摘要和启动命令 JSON")
    args = parser.parse_args()

    try:
        if args.diagnostics:
            report = diagnostics(resolve_data_dir(args.data_dir))
            show_diagnostics(report)
            return 1 if "database_error" in report else 0

        source = MihomoSource(os.environ.get("CLASH_MONITOR_CONTROLLER"), os.environ.get("CLASH_MONITOR_SECRET"))
        if args.check:
            snapshot = source.sample()
            proxy = [connection for connection in snapshot.connections if connection.route == "proxy"]
            identified = sum(connection.app is not None for connection in proxy)
            show_message(f"Mihomo 已连接。活跃连接 {len(snapshot.connections)} 条，代理 {len(proxy)} 条，已识别应用的代理连接 {identified} 条。")
            return 0

        from clash_monitor.tray import run_dashboard

        from clash_monitor.startup import LoginStartup

        data_dir = resolve_data_dir(args.data_dir)
        try:
            use_default_dir = data_dir == default_data_dir()
        except OSError:
            use_default_dir = False
        startup = LoginStartup(data_dir) if use_default_dir else None
        with single_writer(data_dir):
            store = Store(data_dir / "traffic.sqlite3")
            collector = Collector(source, store)
            collector.start()
            try:
                run_dashboard(store, collector, start_hidden=args.start_hidden, startup=startup)
            finally:
                collector.stop()
        return 0
    except AlreadyRunning:
        show_message("流量监控已在运行，请从 Windows 通知区域打开。")
        return 0
    except SourceError as error:
        show_message(str(error), error=True)
    except (OSError, sqlite3.Error):
        show_message("无法启动流量监控，请检查数据目录是否可写。", error=True)
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
