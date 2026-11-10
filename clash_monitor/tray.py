"""Windows notification area and dashboard lifecycle."""

from datetime import datetime
from queue import Empty, SimpleQueue
from threading import Thread
import tkinter as tk

from .ui import Dashboard, INK, format_bytes


def _tray_image():
    from PIL import Image, ImageDraw

    icon = Image.new("RGBA", (64, 64))
    draw = ImageDraw.Draw(icon)
    draw.rounded_rectangle((4, 4, 60, 60), radius=12, fill=INK)
    draw.line((15, 47, 49, 47), fill="#9ac9d5", width=3)
    for x, y in ((17, 35), (28, 28), (39, 18)):
        draw.rounded_rectangle((x, y, x + 7, 43), radius=2, fill="#c5f2f3")
    return icon


def _bind_notification_click(icon, callback):
    # pystray 0.19.5 handles icon clicks but does not expose balloon activation.
    from pystray._util import win32

    original = icon._message_handlers[win32.WM_NOTIFY]
    balloon_click = win32.WM_USER + 5  # NIN_BALLOONUSERCLICK

    def handle(wparam, lparam):
        if lparam == balloon_click:
            callback()
        else:
            original(wparam, lparam)

    icon._message_handlers[win32.WM_NOTIFY] = handle


def _alert_message(alert):
    start = datetime.fromtimestamp(alert.start).strftime("%m-%d %H:%M:%S")
    end = datetime.fromtimestamp(alert.end).strftime("%m-%d %H:%M:%S")
    apps = "、".join((app or "未知应用")[:25] for app in alert.contributors)
    return f"{start}–{end}：{format_bytes(alert.total)}\n主要应用：{apps}"


def run_dashboard(store, collector, *, start_hidden=False, startup=None):
    """Keep Tk on the calling thread and route tray actions through a queue."""
    import pystray
    from PIL import ImageTk

    root = tk.Tk()
    dashboard = Dashboard(root, store, collector, startup=startup)
    image = _tray_image()
    photo = ImageTk.PhotoImage(image)
    root.iconphoto(True, photo)
    actions = SimpleQueue()
    tray_ready = False
    stopping = False
    latest_alert = None

    def show():
        root.deiconify()
        root.lift()

    def stop():
        nonlocal stopping
        if stopping:
            return
        stopping = True
        try:
            icon.stop()
            worker.join(timeout=1)
        finally:
            try:
                collector.stop()
            finally:
                root.destroy()

    def close():
        if tray_ready:
            root.withdraw()
        else:
            stop()

    def handle_actions():
        nonlocal tray_ready, latest_alert
        if tray_ready and not stopping:
            alert = collector.take_alert()
            if alert is not None:
                latest_alert = alert
                try:
                    icon.update_menu()
                    icon.notify(_alert_message(alert), "代理流量突增")
                except OSError:
                    dashboard.footer_text.set("系统通知暂不可用，可从托盘菜单查看最近提醒。")
        try:
            while not stopping:
                action = actions.get_nowait()
                if action == "show":
                    show()
                elif action == "alert" and latest_alert is not None:
                    show()
                    dashboard.show_alert(latest_alert)
                elif action == "stop":
                    stop()
                elif action == "ready":
                    tray_ready = True
                    dashboard.footer_text.set("未归因流量可能含直连，单独显示。关闭窗口后继续在托盘采集。")
                elif action == "failed":
                    tray_ready = False
                    dashboard.footer_text.set("托盘不可用。关闭窗口将停止采集。")
                    show()
        except Empty:
            pass
        if not stopping:
            root.after(100, handle_actions)

    def setup(tray):
        try:
            tray.visible = True
        except OSError:
            actions.put("failed")
        else:
            actions.put("ready")

    def run_tray():
        try:
            icon.run(setup=setup)
        finally:
            actions.put("failed")

    def refresh():
        if not stopping:
            if root.state() != "withdrawn":
                dashboard.refresh()
            root.after(2000, refresh)

    def check_startup():
        if not tray_ready:
            dashboard.footer_text.set("托盘暂不可用。关闭窗口将停止采集。")
            show()

    icon = pystray.Icon("clash-traffic-monitor", image, "Clash 流量监控", menu=pystray.Menu(
        pystray.MenuItem("打开流量监控", lambda: actions.put("show"), default=True),
        pystray.MenuItem("退出", lambda: actions.put("stop")),
        pystray.MenuItem("查看最近提醒", lambda: actions.put("alert"), enabled=lambda item: latest_alert is not None),
    ))
    _bind_notification_click(icon, lambda: actions.put("alert"))
    root.protocol("WM_DELETE_WINDOW", close)
    if start_hidden:
        root.withdraw()
    worker = Thread(target=run_tray, name="notification-area", daemon=True)
    worker.start()
    root.after(100, handle_actions)
    root.after(2000, refresh)
    root.after(4000, check_startup)
    try:
        root.mainloop()
    finally:
        if not stopping:
            stop()
