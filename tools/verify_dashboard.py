"""Exercise the dashboard on real Tk and the optional Win32 tray backend."""

from datetime import date, datetime, time, timedelta
from pathlib import Path
from tempfile import TemporaryDirectory
from tkinter import ttk
import sys
import tkinter as tk

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from clash_monitor.alerts import AlertSettings, SpikeAlert
from clash_monitor.collector import Status
from clash_monitor.storage import Store
from clash_monitor.ui import Dashboard, format_bytes
from clash_monitor.tray import run_dashboard


class Collector:
    stopped = False
    pending_alert = None
    settings = AlertSettings()

    def alert_settings(self):
        return self.settings

    def set_alert_settings(self, settings):
        self.settings = settings

    def take_alert(self):
        alert, self.pending_alert = self.pending_alert, None
        return alert

    def status(self):
        return Status(connected=True, last_sample_at=None, message="")

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
        if hour == 9:
            store.record(timestamp - 1, {}, (0, 0), timestamp - 91)
        store.record(timestamp, traffic.get(hour, {}), unattributed, covered_from=timestamp - 1)


class Startup:
    def __init__(self):
        self.value = False
        self.changes = []

    def enabled(self):
        return self.value

    def set_enabled(self, value):
        self.changes.append(value)
        self.value = value


def verify_settings(dashboard, startup):
    dashboard._show_settings()
    window = next(widget for widget in dashboard.root.winfo_children() if isinstance(widget, tk.Toplevel))
    controls = list(_widgets(window))
    startup_toggle, alert_toggle = [control for control in controls if isinstance(control, ttk.Checkbutton)]
    assert startup.changes == [] and not startup_toggle.instate(["selected"])
    startup_toggle.invoke()
    startup_toggle.invoke()
    assert startup.changes == [True, False]
    alert_toggle.invoke()
    threshold = next(control for control in controls if isinstance(control, ttk.Entry))
    threshold.delete(0, "end")
    threshold.insert(0, "32")
    next(control for control in controls
         if isinstance(control, ttk.Button) and control.cget("text") == "保存提醒设置").invoke()
    assert dashboard.collector.alert_settings() == AlertSettings(True, 32)
    assert not window.winfo_exists()


def verify_hover(dashboard, store, yesterday):
    root = dashboard.root
    dashboard.select_date(yesterday)
    root.update()
    def move(index, y=60):
        left, _, step = dashboard._plot_geometry()
        dashboard.chart.event_generate("<Motion>", x=round(left + step * (index + .5)), y=y)
        root.update()
        return " ".join(dashboard.chart.itemcget(i, "text") for i in dashboard.chart.find_withtag("hover")
                        if dashboard.chart.type(i) == "text")
    assert "未采集" in move(0)
    assert "代理流量 0 B" in move(10)
    assert "09:00–10:00" in move(9) and "下载" in move(9)
    stamp = datetime.combine(yesterday, time(9, 6)).timestamp()
    before = move(9)
    store.record(stamp, {"Cursor.exe": (17, 12345)}, (0, 0), covered_from=stamp-1)
    dashboard.refresh()
    after = " ".join(dashboard.chart.itemcget(i, "text") for i in dashboard.chart.find_withtag("hover")
                     if dashboard.chart.type(i) == "text")
    assert before != after and dashboard._hover == 9
    move(9, 0)
    assert not dashboard.chart.find_withtag("hover")
    move(23)
    box = dashboard.chart.bbox("hover")
    assert box[0] >= 0 and box[2] <= dashboard.chart.winfo_width()
    dashboard.chart.event_generate("<Leave>")
    assert not dashboard.chart.find_withtag("hover")
    for index in (1, 2, 3, 4):
        dashboard.trend_picker.current(index)
        dashboard._trend_change()
        assert dashboard._hover is None
        text = move(len(dashboard.days)-1)
        assert date.today().isoformat() in text
    dashboard.select_date(date.today())
    if datetime.now().hour < 23:
        assert "尚未开始" in move(23)
    dashboard.select_date(yesterday)
    print("PASS hover: full columns, empty/zero/value/future, stationary refresh, edges, leave and all trend switches")


def verify_dashboard(store, yesterday):
    root = tk.Tk()
    try:
        startup = Startup()
        dashboard = Dashboard(root, store, Collector(), startup=startup)
        root.update()
        assert dashboard.day == date.today()
        assert "覆盖 0.00 分钟" in dashboard.day_quality.get()
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
        assert "代理流量 0 B" in dashboard.hour_detail.get()
        dashboard.select_hour(0)
        assert "未采集" in dashboard.hour_detail.get()
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
        for index, count in ((1, 7), (2, 30)):
            dashboard.trend_picker.current(index)
            dashboard.trend_picker.event_generate("<<ComboboxSelected>>")
            root.update()
            assert len(dashboard.days) == count and dashboard.hour_picker.instate(["disabled"])
            assert len(dashboard.table.get_children()) == 4
            left, _, step = dashboard._plot_geometry()
            dashboard.chart.event_generate("<Button-1>", x=round(left + step * (count - 1.5)), y=60)
            root.update()
            assert dashboard.day == yesterday and dashboard.hour == 13
            assert dashboard.trend_index == 0 and dashboard.hour_picker.instate(["!disabled"])
        today = date.today()
        for index, first in ((3, today - timedelta(days=today.weekday())), (4, today.replace(day=1))):
            dashboard.trend_picker.current(index)
            dashboard.trend_picker.event_generate("<<ComboboxSelected>>")
            root.update()
            assert dashboard.days[0].day == first and dashboard.days[-1].day == today
            expected = 4 if first <= yesterday else 0
            assert len(dashboard.table.get_children()) == expected
        dashboard.select_date(yesterday)
        dashboard.select_hour(9)
        dashboard.minute_picker.current(2)
        dashboard.minute_picker.event_generate("<<ComboboxSelected>>")
        root.update()
        assert "09:05–09:10" in dashboard.hour_detail.get()
        assert [dashboard.table.item(i, "values")[1] for i in dashboard.table.get_children()] == ["Cursor.exe", "未知应用"]
        dashboard.minute_picker.current(3)
        dashboard.minute_picker.event_generate("<<ComboboxSelected>>")
        root.update()
        assert "无明细" in dashboard.hour_detail.get() and not dashboard.table.get_children()
        dashboard.select_hour(13)
        assert dashboard.minute is None
        verify_hover(dashboard, store, yesterday)
        verify_settings(dashboard, startup)
        print("PASS Tk settings: opt-in startup using a substitute, alert threshold and enable switch")
        print("PASS minute detail: hour drilldown, matching apps, empty bucket and hour reset")
        print("PASS calendar week/month rankings: local Monday and first-of-month boundaries")
        print("PASS 7/30 day trends: cumulative ranking, date click drills down to hourly ranking")
        print("PASS SQLite dashboard: date changes, invalid date, click and keyboard hour selection, ranking, unknown app, gaps")
    finally:
        root.destroy()


def _widgets(parent):
    for child in parent.winfo_children():
        yield child
        yield from _widgets(child)


def verify_tray(store, yesterday):
    import pystray
    from pystray._util import win32

    original_tk, original_icon = tk.Tk, pystray.Icon
    icons = []
    errors = []
    notifications = []
    peak = datetime.combine(yesterday, time(9, 5)).timestamp()
    collector = Collector()
    collector.pending_alert = SpikeAlert(peak - 480, peak + 420, 3 * 1024**2, ("Cursor.exe", None), peak)

    class ProbeIcon(original_icon):
        def __init__(self, *args, **kwargs):
            super().__init__(*args, **kwargs)
            icons.append(self)

        def notify(self, message, title=None):
            super().notify(message, title)
            notifications.append((message, title))

    class ProbeTk(original_tk):
        def __init__(self):
            super().__init__()
            self.after(750, self.check_notification)
            self.after(3500, self.watchdog)

        def report_callback_exception(self, typ, value, trace):
            errors.append(repr(value))
            self.quit()

        def watchdog(self):
            errors.append("watchdog timeout")
            self.quit()

        def check_notification(self):
            icon = icons[0]
            assert icon.visible and self.state() == "withdrawn"
            assert len(notifications) == 1
            message, title = notifications[0]
            assert "Cursor.exe" in message and "3.00 MiB" in message and title == "代理流量突增"
            # Exercise the native message pump with the Shell's click message.
            assert win32.PostMessage(icon._hwnd, win32.WM_NOTIFY, 0, win32.WM_USER + 5)
            self.after(350, self.check_hide)

        def check_hide(self):
            icon = icons[0]
            assert icon.visible and self.state() == "normal"
            labels = [str(child.cget("textvariable")) for child in _widgets(self)
                      if isinstance(child, ttk.Label) and child.cget("textvariable")]
            assert any("09:05–09:10" in self.getvar(name) for name in labels)
            self.tk.call(self.protocol("WM_DELETE_WINDOW"))
            assert self.state() == "withdrawn"
            next(iter(icon.menu))(icon)
            self.after(350, self.check_show)

        def check_show(self):
            assert self.state() == "normal"
            list(icons[0].menu)[1](icons[0])

    tk.Tk, pystray.Icon = ProbeTk, ProbeIcon
    try:
        run_dashboard(store, collector, start_hidden=True)
        assert not errors, errors
        assert collector.stopped and not icons[0]._running
        print("PASS Win32 alert: notification submitted, native click message opens the matching five-minute detail")
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
            verify_tray(store, yesterday)
