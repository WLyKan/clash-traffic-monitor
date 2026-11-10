"""Windows notification area and dashboard lifecycle."""

from queue import Empty, SimpleQueue
from threading import Thread
import tkinter as tk

from .ui import Dashboard, INK


def _tray_image():
    from PIL import Image, ImageDraw

    icon = Image.new("RGBA", (64, 64))
    draw = ImageDraw.Draw(icon)
    draw.rounded_rectangle((4, 4, 60, 60), radius=12, fill=INK)
    draw.line((15, 47, 49, 47), fill="#9ac9d5", width=3)
    for x, y in ((17, 35), (28, 28), (39, 18)):
        draw.rounded_rectangle((x, y, x + 7, 43), radius=2, fill="#c5f2f3")
    return icon


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
        nonlocal tray_ready
        try:
            while not stopping:
                action = actions.get_nowait()
                if action == "show":
                    show()
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
    ))
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
