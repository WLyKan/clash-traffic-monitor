from dataclasses import dataclass
import sqlite3
import threading
import time

from .source import MihomoSource, Snapshot, SourceError
from .storage import Store


@dataclass(frozen=True)
class Status:
    connected: bool
    message: str
    last_sample_at: float | None
    gap_tolerance: float = 3.5


def traffic_delta(previous: Snapshot, current: Snapshot):
    before = {connection.id: connection for connection in previous.connections}
    previous_end = previous.completed_at or previous.sampled_at
    apps = {}
    observed_up = observed_down = unknown_up = unknown_down = 0
    for connection in current.connections:
        older = before.get(connection.id)
        if older is not None:
            up = max(0, connection.uploaded - older.uploaded)
            down = max(0, connection.downloaded - older.downloaded)
        elif connection.started_at is not None and connection.started_at >= previous_end:
            up, down = connection.uploaded, connection.downloaded
        else:
            continue
        observed_up += up
        observed_down += down
        if connection.route == "proxy" and (up or down):
            app_up, app_down = apps.get(connection.app, (0, 0))
            apps[connection.app] = (app_up + up, app_down + down)
        elif connection.route == "unknown":
            unknown_up += up
            unknown_down += down

    # Keep residuals signed so adjacent non-atomic samples can cancel within the hour.
    unmatched = (
        current.upload_total - previous.upload_total - observed_up,
        current.download_total - previous.download_total - observed_down,
    )
    return apps, (unknown_up, unknown_down), unmatched


class Collector:
    def __init__(self, source: MihomoSource, store: Store, interval: float = 1.0):
        self.source = source
        self.store = store
        self.interval = interval
        self._stop = threading.Event()
        self._lock = threading.Lock()
        self._last_sample = store.last_sample()
        self._status = Status(False, "正在连接 Mihomo", self._last_sample, self.interval * 3.5)
        self._previous = None
        self._thread = threading.Thread(target=self._run, name="traffic-collector", daemon=True)

    def start(self) -> None:
        self._thread.start()

    def stop(self) -> None:
        self._stop.set()
        if self._thread.is_alive():
            self._thread.join(timeout=6)

    def status(self) -> Status:
        with self._lock:
            return self._status

    def _set_status(self, connected: bool, message: str) -> None:
        with self._lock:
            self._status = Status(connected, message, self._last_sample, self.interval * 3.5)

    def poll(self) -> None:
        try:
            current = self.source.sample()
            previous = self._previous
            continuous = (
                previous is not None
                and 0 < current.sampled_at - previous.sampled_at <= self.interval * 3.5
                and current.generation == previous.generation
                and current.upload_total >= previous.upload_total
                and current.download_total >= previous.download_total
            )
            if continuous:
                apps, unknown_route, unmatched = traffic_delta(previous, current)
                self.store.record(
                    current.sampled_at, apps, unknown_route,
                    unmatched=unmatched, covered_from=previous.sampled_at,
                )
            else:
                self.store.record(current.sampled_at, {}, (0, 0), self._last_sample)
        except SourceError as error:
            self._previous = None
            self._set_status(False, str(error))
            return
        except (OSError, sqlite3.Error):
            self._previous = None
            self._set_status(False, "无法写入本地流量数据库，请检查数据目录")
            return

        self._previous = current
        self._last_sample = current.sampled_at
        self._set_status(True, "正在采集代理流量")

    def _run(self) -> None:
        while not self._stop.is_set():
            started = time.monotonic()
            self.poll()
            self._stop.wait(max(0, self.interval - (time.monotonic() - started)))
