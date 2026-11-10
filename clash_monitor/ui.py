"""Native Windows usage dashboard."""

from datetime import date, datetime, timedelta
from sqlite3 import Error as DatabaseError
import time
import tkinter as tk
from tkinter import messagebox, ttk


BACKGROUND = "#eff4f9"
PAPER = "#ffffff"
INK = "#15253a"
MUTED = "#465b74"
LINE = "#d5deea"
DOWNLOAD = "#12687d"
UPLOAD = "#4168b1"
ACTIVE = "#e0f0f6"
GAP = "#a95216"
MISSING = "#c0cbd9"
HOUR_LABELS = tuple(f"{hour:02d}:00" for hour in range(24))
TREND_LABELS = ("24 小时代理趋势", "最近 7 天", "最近 30 天", "本周", "本月")


def trend_start(index, today):
    if index == 3:
        return today - timedelta(days=today.weekday())
    if index == 4:
        return today.replace(day=1)
    return today - timedelta(days=(7, 30)[index - 1] - 1)


def format_bytes(value):
    for unit in ("B", "KiB", "MiB", "GiB", "TiB"):
        if value < 1024 or unit == "TiB":
            return f"{value:g} B" if unit == "B" else f"{value:,.2f} {unit}"
        value /= 1024


def format_gap(seconds):
    minutes, seconds = divmod(max(1, round(seconds)), 60)
    hours, minutes = divmod(minutes, 60)
    if hours:
        return f"{hours} 小时 {minutes} 分钟"
    return f"{minutes} 分 {seconds:02d} 秒" if minutes else f"{seconds} 秒"


class Dashboard:
    def __init__(self, root, store, collector, *, startup=None):
        self.root = root
        self.store = store
        self.collector = collector
        self.startup = startup
        self.day = date.today()
        self.hour = datetime.now().hour
        self.hours = []
        self.days = []
        self.trend_index = 0
        self.focus_day = 0
        self._table_rows = None
        self._chart_items = None

        root.title("Clash 流量监控")
        width = min(900, root.winfo_screenwidth() - 90)
        height = min(690, root.winfo_screenheight() - 85)
        root.geometry(f"{width}x{height}")
        root.minsize(min(690, width), min(585, height))
        root.configure(background=BACKGROUND)
        root.grid_columnconfigure(0, weight=1)
        root.grid_rowconfigure(0, weight=1)
        self._styles()

        body = ttk.Frame(root, style="Surface.TFrame", padding=(22, 14, 22, 11))
        body.grid(row=0, column=0, sticky="nsew")
        body.grid_columnconfigure(0, weight=1)
        body.grid_rowconfigure(4, weight=1)

        title = ttk.Frame(body, style="Surface.TFrame")
        title.grid(row=0, column=0, sticky="ew", pady=(0, 9))
        ttk.Label(title, text="Clash 流量监控", style="Title.TLabel").pack(side="left")
        self.status_text = tk.StringVar()
        self.status_label = ttk.Label(
            title, textvariable=self.status_text, style="Status.TLabel", justify="right", wraplength=365
        )
        self.status_label.pack(side="right", anchor="s", pady=(0, 3))

        toolbar = ttk.Frame(body, style="Surface.TFrame")
        toolbar.grid(row=1, column=0, sticky="ew", pady=(0, 10))
        ttk.Button(toolbar, text="上一天", width=7, command=lambda: self.step_day(-1)).pack(side="left")
        ttk.Button(toolbar, text="今天", width=6, command=lambda: self.select_date(date.today())).pack(
            side="left", padx=4
        )
        self.next_day_button = ttk.Button(toolbar, text="下一天", width=7, command=lambda: self.step_day(1))
        self.next_day_button.pack(side="left")
        ttk.Label(toolbar, text="日期", style="Muted.TLabel").pack(side="left", padx=(16, 6))
        self.date_text = tk.StringVar(value=self.day.isoformat())
        self.date_entry = ttk.Entry(toolbar, width=12, textvariable=self.date_text)
        self.date_entry.pack(side="left")
        self.date_entry.bind("<Return>", self.apply_date)
        ttk.Button(toolbar, text="查看", width=6, command=self.apply_date).pack(side="left", padx=4)
        ttk.Button(toolbar, text="设置", command=self._show_settings).pack(side="right")
        self.date_error = tk.StringVar()
        ttk.Label(toolbar, textvariable=self.date_error, style="Error.TLabel").pack(side="left", padx=5)

        metrics = ttk.Frame(body, style="Paper.TFrame", padding=(14, 8))
        metrics.grid(row=2, column=0, sticky="ew", pady=(0, 10))
        self.day_total, self.day_download, self.day_upload = (tk.StringVar() for _ in range(3))
        for index, (name, value) in enumerate(
            (("代理合计", self.day_total), ("下载", self.day_download), ("上传", self.day_upload))
        ):
            metrics.grid_columnconfigure(index, weight=1, uniform="metric")
            ttk.Label(metrics, text=name, style="Caption.TLabel").grid(row=0, column=index, sticky="w")
            ttk.Label(metrics, textvariable=value, style="Value.TLabel").grid(row=1, column=index, sticky="w")

        trend = ttk.Frame(body, style="Paper.TFrame", padding=(14, 8, 14, 9))
        trend.grid(row=3, column=0, sticky="ew", pady=(0, 10))
        trend.grid_columnconfigure(0, weight=1)
        self.trend_picker = ttk.Combobox(trend, width=19, state="readonly", values=TREND_LABELS)
        self.trend_picker.current(0)
        self.trend_picker.grid(row=0, column=0, sticky="w")
        self.trend_picker.bind("<<ComboboxSelected>>", self._trend_change)
        legend = ttk.Frame(trend, style="Paper.TFrame")
        legend.grid(row=0, column=0, sticky="e")
        for color, name in ((DOWNLOAD, "下载"), (UPLOAD, "上传"), (GAP, "采集缺口")):
            swatch = tk.Canvas(legend, width=8, height=8, background=PAPER, highlightthickness=0)
            swatch.create_rectangle(0, 0, 8, 8, fill=color, outline="")
            swatch.pack(side="left", padx=(9, 4))
            ttk.Label(legend, text=name, style="Caption.TLabel").pack(side="left")
        self.chart = tk.Canvas(
            trend, height=141, background=PAPER, highlightbackground=PAPER, highlightcolor=DOWNLOAD,
            highlightthickness=1, takefocus=True,
        )
        self.chart.grid(row=1, column=0, sticky="ew", pady=(3, 3))
        self.chart.bind("<Configure>", self._draw_chart)
        self.chart.bind("<Button-1>", self._chart_click)
        self.chart.bind("<Left>", lambda event: self._chart_key(self._chart_selection() - 1))
        self.chart.bind("<Right>", lambda event: self._chart_key(self._chart_selection() + 1))
        self.chart.bind("<Home>", lambda event: self._chart_key(0))
        self.chart.bind("<End>", lambda event: self._chart_key(self._chart_count() - 1))
        self.chart.bind("<Return>", self._open_focused_day)
        self.day_quality = tk.StringVar()
        ttk.Label(trend, textvariable=self.day_quality, style="Caption.TLabel").grid(row=2, column=0, sticky="w")

        ranking = ttk.Frame(body, style="Paper.TFrame", padding=(14, 8, 14, 10))
        ranking.grid(row=4, column=0, sticky="nsew")
        ranking.grid_columnconfigure(0, weight=1)
        ranking.grid_rowconfigure(2, weight=1)
        ranking_title = ttk.Frame(ranking, style="Paper.TFrame")
        ranking_title.grid(row=0, column=0, columnspan=2, sticky="ew")
        self.ranking_title = tk.StringVar(value="应用排行")
        ttk.Label(ranking_title, textvariable=self.ranking_title, style="Heading.TLabel").pack(side="left")
        self.hour_text = tk.StringVar(value=HOUR_LABELS[self.hour])
        self.hour_picker = ttk.Combobox(
            ranking_title, width=7, state="readonly", values=HOUR_LABELS, textvariable=self.hour_text
        )
        self.hour_picker.pack(side="right")
        ttk.Label(ranking_title, text="小时", style="Caption.TLabel").pack(side="right", padx=6)
        self.hour_picker.bind("<<ComboboxSelected>>", self._picker_change)
        self.hour_detail = tk.StringVar()
        ttk.Label(ranking, textvariable=self.hour_detail, style="Caption.TLabel").grid(
            row=1, column=0, columnspan=2, sticky="w", pady=(3, 5)
        )
        self.table = ttk.Treeview(
            ranking, columns=("rank", "app", "down", "up", "total"), show="headings", height=4
        )
        for key, title, size, stretch in (
            ("rank", "排名", 48, False), ("app", "应用", 230, True), ("down", "下载", 103, False),
            ("up", "上传", 103, False), ("total", "合计", 108, False),
        ):
            anchor = "w" if key == "app" else "e"
            self.table.heading(key, text=title, anchor=anchor)
            self.table.column(key, width=size, minwidth=size, anchor=anchor, stretch=stretch)
        self.table.tag_configure("alternate", background="#f1f6fb")
        self.table.grid(row=2, column=0, sticky="nsew")
        scrollbar = ttk.Scrollbar(ranking, orient="vertical", command=self.table.yview)
        self.table.configure(yscrollcommand=scrollbar.set)
        scrollbar.grid(row=2, column=1, sticky="ns")

        self.footer_text = tk.StringVar(value="未归因流量可能含直连，单独显示。关闭窗口后继续在托盘采集。")
        ttk.Label(body, textvariable=self.footer_text, style="Muted.TLabel").grid(
            row=5, column=0, sticky="w", pady=(8, 0)
        )
        self.refresh()

    def _show_settings(self):
        window = tk.Toplevel(self.root)
        window.title("设置")
        window.transient(self.root)
        settings = ttk.Frame(window, padding=16)
        settings.pack(fill="both", expand=True)
        if self.startup is None:
            ttk.Label(settings, text="独立数据目录模式下，登录自启不可用。").pack(anchor="w")
        else:
            try:
                enabled = tk.BooleanVar(window, self.startup.enabled())
            except OSError:
                ttk.Label(settings, text="无法读取当前用户的登录启动设置。").pack(anchor="w")
            else:
                def change_startup():
                    requested = enabled.get()
                    try:
                        self.startup.set_enabled(requested)
                    except OSError:
                        enabled.set(not requested)
                        messagebox.showerror("设置", "无法写入当前用户的登录启动设置。", parent=window)

                ttk.Checkbutton(
                    settings, text="登录 Windows 后启动并收起到托盘",
                    variable=enabled, command=change_startup,
                ).pack(anchor="w")
        ttk.Button(settings, text="关闭", command=window.destroy).pack(anchor="e", pady=(12, 0))
        window.grab_set()

    def _styles(self):
        style = ttk.Style(self.root)
        base = ("Microsoft YaHei UI", 10)
        style.configure("Surface.TFrame", background=BACKGROUND)
        style.configure("Paper.TFrame", background=PAPER)
        style.configure("Title.TLabel", font=("Microsoft YaHei UI", 20, "bold"), background=BACKGROUND, foreground=INK)
        style.configure("Heading.TLabel", font=("Microsoft YaHei UI", 11, "bold"), background=PAPER, foreground=INK)
        style.configure("Caption.TLabel", font=base, background=PAPER, foreground=MUTED)
        style.configure("Value.TLabel", font=("Segoe UI", 17, "bold"), background=PAPER, foreground=INK)
        style.configure("Muted.TLabel", font=base, background=BACKGROUND, foreground=MUTED)
        style.configure("Error.TLabel", font=base, background=BACKGROUND, foreground=GAP)
        style.configure("Status.TLabel", font=base, background=BACKGROUND, foreground=DOWNLOAD)
        style.configure("Treeview", font=base, rowheight=max(25, round(18 * float(self.root.tk.call("tk", "scaling")))))
        style.configure("Treeview.Heading", font=base)
        style.configure("TButton", font=base)

    def apply_date(self, event=None):
        entered = self.date_text.get().strip()
        try:
            day = date.fromisoformat(entered)
        except ValueError:
            self.date_error.set("请输入 YYYY-MM-DD")
            return
        if day.isoformat() != entered:
            self.date_error.set("请输入 YYYY-MM-DD")
        elif day > date.today():
            self.date_error.set("请选择今天或更早的日期")
        else:
            self.select_date(day)

    def step_day(self, offset):
        if offset < 0 and self.day == date.min:
            return
        next_day = self.day + timedelta(days=offset)
        if next_day <= date.today():
            self.select_date(next_day)

    def select_date(self, day):
        self.trend_index = 0
        self.trend_picker.current(0)
        self.day = day
        self.date_text.set(day.isoformat())
        self.date_error.set("")
        self.hour = datetime.now().hour if day == date.today() else 0
        self.refresh(pick_historical_hour=day < date.today())

    def _trend_change(self, event=None):
        self.trend_index = self.trend_picker.current()
        self.focus_day = 0
        self.refresh()
        if self.days:
            self.focus_day = len(self.days) - 1
            self._draw_chart()

    def _open_focused_day(self, event=None):
        if self.trend_index and self.days:
            self.select_date(self.days[self.focus_day].day)
        return "break"

    def select_hour(self, hour):
        self.hour = max(0, min(23, hour))
        self.hour_text.set(HOUR_LABELS[self.hour])
        self.refresh()

    def _picker_change(self, event):
        self.select_hour(self.hour_picker.current())

    def refresh(self, *, pick_historical_hour=False):
        self.next_day_button.state(["disabled"] if self.day >= date.today() else ["!disabled"])
        status = self.collector.status()
        last = "" if status.last_sample_at is None else datetime.fromtimestamp(status.last_sample_at).strftime(" %H:%M:%S")
        if status.connected:
            self.status_text.set(f"采集中{last}")
            self.status_label.configure(style="Status.TLabel")
        else:
            problem = status.message or "等待连接 Clash"
            self.status_text.set(f"采集离线 · 上次{last}\n{problem}" if last else problem)
            self.status_label.configure(style="Error.TLabel")
        self.ranking_title.set("范围应用累计排行" if self.trend_index else "应用排行")
        self.hour_picker.state(["disabled"] if self.trend_index else ["!disabled", "readonly"])
        try:
            if self.trend_index:
                end = date.today()
                start = trend_start(self.trend_index, end)
                self.days, apps = self.store.range_traffic(start, end)
                self.focus_day = min(self.focus_day, len(self.days) - 1)
                self._render_summary()
                self._draw_chart()
                self.hour_detail.set(f"{start:%Y-%m-%d} 至 {end:%Y-%m-%d}　{len(apps)} 项　选择日期查看小时")
                self._render_rows(apps)
                return
            self.days = []
            self.hours = self.store.day_hours(
                self.day, as_of=time.time() if status.last_sample_at is not None else None,
                gap_tolerance=status.gap_tolerance if status.connected else 0,
            )
            if pick_historical_hour and self.hours:
                chosen = max(self.hours, key=lambda row: (row.uploaded + row.downloaded, row.samples > 0))
                self.hour = chosen.hour
            self.hour_text.set(HOUR_LABELS[self.hour])
            self._render_summary()
            self._draw_chart()
            self._render_rank(self.store.hour_apps(self.day, self.hour))
        except DatabaseError:
            self.days = []
            self.hours = []
            self._render_summary()
            self._draw_chart()
            self._render_rank([])
            self.day_quality.set("暂时无法读取本地流量记录")

    def _render_summary(self):
        rows = self.days if self.trend_index else self.hours
        uploaded = sum(row.uploaded for row in rows)
        downloaded = sum(row.downloaded for row in rows)
        covered = sum(row.covered_seconds for row in rows)
        legacy = any(row.legacy_coverage for row in rows)
        gaps = sum(row.gap_seconds for row in rows)
        self.day_total.set(format_bytes(uploaded + downloaded))
        self.day_download.set(format_bytes(downloaded))
        self.day_upload.set(format_bytes(uploaded))
        gap_text = f"　采集缺口 {format_gap(gaps)}" if gaps else ""
        unknown = "（含旧记录，精确覆盖未知）" if legacy else ""
        detail = ""
        if not self.trend_index:
            unattributed = sum(row.unattributed_up + row.unattributed_down for row in rows)
            detail = f"　未归因流量 {format_bytes(unattributed)}"
        self.day_quality.set(f"采集覆盖 {covered / 60:.2f} 分钟{unknown}{detail}{gap_text}")

    def _chart_selection(self):
        return self.focus_day if self.trend_index else self.hour

    def _chart_count(self):
        return max(1, len(self.days)) if self.trend_index else 24

    def _plot_geometry(self):
        left, right = 76, self.chart.winfo_width() - 7
        return left, right, (right - left) / self._chart_count()

    def _draw_chart(self, event=None):
        chart = self.chart
        left, right, step = self._plot_geometry()
        if step <= 0:
            return
        top, bottom = 12, chart.winfo_height() - 26
        rows = dict(enumerate(self.days)) if self.trend_index else {row.hour: row for row in self.hours}
        snapshot = tuple(
            (index, row.uploaded, row.downloaded, bool(row.covered_seconds or row.legacy_coverage),
             bool(row.gap_seconds)) for index, row in rows.items()
        )
        selected = self._chart_selection()
        key = (snapshot, tuple(row.day for row in self.days), self.trend_index, selected, left, right, bottom)
        if key == self._chart_items:
            return
        self._chart_items = key
        chart.delete("all")
        high = max((row.uploaded + row.downloaded for row in rows.values()), default=0)
        scale = max(1, high)
        axis_font = ("Segoe UI", 8)
        for ratio in (0, 0.5, 1):
            y = bottom - (bottom - top) * ratio
            chart.create_line(left, y, right, y, fill=LINE, dash=(2, 4) if ratio else ())
            if high or ratio == 0:
                chart.create_text(left - 7, y, anchor="e", text=format_bytes(round(high * ratio)), fill=MUTED, font=axis_font)
        if self.trend_index and not self.days:
            return
        for index in range(self._chart_count()):
            x = left + index * step
            if index == selected:
                chart.create_rectangle(x + 1, top, x + step - 1, bottom + 20, fill=ACTIVE, outline="")
            row = rows.get(index)
            center = x + step / 2
            width = min(step * 0.56, 23)
            if row and (row.covered_seconds or row.legacy_coverage or row.uploaded or row.downloaded):
                y = bottom
                for value, color in ((row.downloaded, DOWNLOAD), (row.uploaded, UPLOAD)):
                    if value:
                        height = max(1, (bottom - top) * value / scale)
                        chart.create_rectangle(center - width / 2, y - height, center + width / 2, y, fill=color, outline="")
                        y -= height
                if row.downloaded + row.uploaded == 0:
                    chart.create_line(center - width / 2, bottom, center + width / 2, bottom, fill=DOWNLOAD)
            else:
                chart.create_line(center - width / 2, bottom, center + width / 2, bottom, fill=MISSING, dash=(2, 2))
            if row and row.gap_seconds:
                chart.create_line(center - width / 2, bottom + 3, center + width / 2, bottom + 3, fill=GAP, width=2)
            if self.trend_index:
                # Leave room for the focused date label between the regular ticks.
                show_label = index == selected or (
                    index % (1 if len(self.days) <= 7 else 5) == 0 and
                    abs(index - selected) * step >= 35
                )
                label = self.days[index].day.strftime("%m-%d")
            else:
                show_label = index % 3 == 0 or index == selected
                label = f"{index:02d}"
            if show_label:
                chart.create_text(center, bottom + 12, text=label, fill=INK if index == selected else MUTED, font=axis_font)

    def _chart_click(self, event):
        left, right, step = self._plot_geometry()
        if step > 0 and left <= event.x < right:
            self.chart.focus_set()
            index = int((event.x - left) / step)
            if self.trend_index:
                self.focus_day = index
                self._open_focused_day()
            else:
                self.select_hour(index)

    def _chart_key(self, index):
        if self.trend_index:
            self.focus_day = max(0, min(self._chart_count() - 1, index))
            self._draw_chart()
        else:
            self.select_hour(index)
        return "break"

    def _render_rank(self, apps):
        hour = next((row for row in self.hours if row.hour == self.hour), None)
        period = f"{HOUR_LABELS[self.hour]}–{self.hour + 1:02d}:00"
        if apps:
            description = f"{len(apps)} 项　代理合计 {format_bytes(hour.uploaded + hour.downloaded)}"
        elif hour and hour.covered_seconds:
            description = "已采集，代理流量 0 B"
        elif hour and hour.legacy_coverage:
            description = "旧记录，代理合计 0 B"
        elif hour and hour.samples:
            description = "已建立采样基线"
        else:
            description = "该时段未采集"
        if hour and (hour.samples or hour.covered_seconds):
            description += f"　覆盖 {hour.covered_seconds / 60:.2f} 分钟"
            if hour.legacy_coverage:
                description += "（旧记录覆盖未知）"
        if hour and hour.gap_seconds:
            description += f"　缺口 {format_gap(hour.gap_seconds)}"
        if hour and (hour.unattributed_up or hour.unattributed_down):
            description += f"　未归因 {format_bytes(hour.unattributed_up + hour.unattributed_down)}"
        self.hour_detail.set(f"{period}　{description}")
        self._render_rows(apps)

    def _render_rows(self, apps):
        values = tuple((app.app, app.uploaded, app.downloaded) for app in apps)
        key = (self.trend_index, self.day, self.hour, values)
        if key == self._table_rows:
            return
        self._table_rows = key
        offset = self.table.yview()[0]
        self.table.delete(*self.table.get_children())
        for rank, (app, uploaded, downloaded) in enumerate(values, 1):
            self.table.insert(
                "", "end", values=(rank, app or "未知应用", format_bytes(downloaded), format_bytes(uploaded),
                                  format_bytes(uploaded + downloaded)),
                tags=("alternate",) if rank % 2 == 0 else (),
            )
        self.table.yview_moveto(offset)
