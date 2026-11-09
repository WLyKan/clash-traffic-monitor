"""Exercise the dashboard on real Tk and the optional Win32 tray backend."""

from datetime import date, datetime, time, timedelta
from pathlib import Path
from tempfile import TemporaryDirectory
from types import SimpleNamespace
import sys
import tkinter as tk

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from clash_monitor.storage import Store
from clash_monitor.ui import Dashboard, format_bytes
from clash_monitor.tray import run_dashboard


class Collector:
    stopped = False

    def status(self):
        return SimpleNamespace(connected=True, last_sample_at=None, message="")

    def stop(self):
        self.stopped = True


def seed_day(store, day):
    traffic = {
        8: {"msedge.exe": (524288, 16777216)},
        9: {"Cursor.exe": (589824, 17825792), None: (0, 1048576)},
        13: {"ChatGPT.exe": (851968, 27262976)},
    }
    for hour in range(6, 16):
        timestamp = datetime.combine(day, time(hour, 5)).timestamp()
        unattributed = (4096, 768) if hour == 9 else (0, 0)
        store.record(timestamp, traffic.get(hour, {}), unattributed, timestamp - 90 if hour == 9 else None)


def verify_dashboard(store, yesterday):
    root = tk.Tk()
    try:
        dashboard = Dashboard(root, store, Collector())
        root.update()
        assert dashboard.day == date.today()
        assert "0/24" in dashboard.day_quality.get()
        dashboard.date_text.set(yesterday.isoformat())
        dashboard.apply_date()
        root.update()
        assert dashboard.day == yesterday and dashboard.hour == 13
        assert dashboard.table.item(dashboard.table.get_children()[0], "values")[1] == "ChatGPT.exe"
        left, _, step = dashboard._plot_geometry()
        dashboard.chart.event_generate("<Button-1>", x=round(left + step * 9.5), y=60)
        root.update()
        assert dashboard.hour == 9
        assert [dashboard.table.item(i, "values")[1] for i in dashboard.table.get_children()] == ["Cursor.exe", "未知应用"]
        assert format_bytes(1024**3) == "1.00 GiB"
        assert "4.75 KiB" in dashboard.hour_detail.get() and "1 分 30 秒" in dashboard.hour_detail.get()
        dashboard.chart.focus_force()
        root.update()
        dashboard.chart.event_generate("<Right>")
        root.update()
        assert dashboard.hour == 10
        dashboard.hour_picker.current(13)
        dashboard.hour_picker.event_generate("<<ComboboxSelected>>")
        root.update()
        assert dashboard.hour == 13
        dashboard.date_text.set("2026-02-31")
        dashboard.apply_date()
        assert dashboard.date_error.get() and dashboard.day == yesterday
        dashboard.select_date(yesterday)
        dashboard.select_hour(9)
        root.update()
        print("PASS SQLite dashboard: date changes, invalid date, click and keyboard hour selection, ranking, unknown app, gaps")
    finally:
        root.destroy()


def verify_tray(store):
    import pystray

    original_tk, original_icon = tk.Tk, pystray.Icon
    icons = []
    errors = []

    class ProbeIcon(original_icon):
        def __init__(self, *args, **kwargs):
            super().__init__(*args, **kwargs)
            icons.append(self)

    class ProbeTk(original_tk):
        def __init__(self):
            super().__init__()
            self.after(750, self.check_hide)
            self.after(3500, self.watchdog)

        def report_callback_exception(self, typ, value, trace):
            errors.append(repr(value))
            self.quit()

        def watchdog(self):
            errors.append("watchdog timeout")
            self.quit()

        def check_hide(self):
            icon = icons[0]
            assert icon.visible and self.state() == "normal"
            self.tk.call(self.protocol("WM_DELETE_WINDOW"))
            assert self.state() == "withdrawn"
            next(iter(icon.menu))(icon)
            self.after(350, self.check_show)

        def check_show(self):
            assert self.state() == "normal"
            list(icons[0].menu)[1](icons[0])

    collector = Collector()
    tk.Tk, pystray.Icon = ProbeTk, ProbeIcon
    try:
        run_dashboard(store, collector)
        assert not errors, errors
        assert collector.stopped and not icons[0]._running
        print("PASS Win32 tray: icon setup, window close hides, default menu restores, exit stops collector and icon")
    finally:
        tk.Tk, pystray.Icon = original_tk, original_icon


if __name__ == "__main__":
    with TemporaryDirectory(dir=Path(__file__).parent, prefix="dashboard-test-") as directory:
        store = Store(Path(directory) / "traffic.sqlite3")
        yesterday = date.today() - timedelta(days=1)
        seed_day(store, yesterday)
        verify_dashboard(store, yesterday)
        if "--tray" in sys.argv:
            verify_tray(store)
