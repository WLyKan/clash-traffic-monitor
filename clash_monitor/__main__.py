import argparse
from contextlib import contextmanager
import ctypes
import msvcrt
import os
from pathlib import Path
import sqlite3
import sys

from clash_monitor.collector import Collector
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


def main() -> int:
    default_dir = Path(os.environ.get("LOCALAPPDATA", Path.home() / "AppData" / "Local"))
    parser = argparse.ArgumentParser(description="Clash 流量监控")
    parser.add_argument("--data-dir", type=Path, default=default_dir / "ClashTrafficMonitor")
    parser.add_argument("--start-hidden", action="store_true", help="启动后只显示通知区域图标")
    parser.add_argument("--check", action="store_true", help="只读检查 Mihomo 连接和应用归因")
    args = parser.parse_args()

    try:
        source = MihomoSource(os.environ.get("CLASH_MONITOR_CONTROLLER"), os.environ.get("CLASH_MONITOR_SECRET"))
        if args.check:
            snapshot = source.sample()
            proxy = [connection for connection in snapshot.connections if connection.route == "proxy"]
            identified = sum(connection.app is not None for connection in proxy)
            show_message(f"Mihomo 已连接。活跃连接 {len(snapshot.connections)} 条，代理 {len(proxy)} 条，已识别应用的代理连接 {identified} 条。")
            return 0

        from clash_monitor.tray import run_dashboard

        with single_writer(args.data_dir):
            store = Store(args.data_dir / "traffic.sqlite3")
            collector = Collector(source, store)
            collector.start()
            try:
                run_dashboard(store, collector, start_hidden=args.start_hidden)
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
