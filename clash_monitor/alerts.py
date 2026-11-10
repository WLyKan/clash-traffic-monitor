"""Detect configured traffic spikes in a continuously covered rolling window."""

from collections import deque
from dataclasses import dataclass

from .storage import minute_bucket


WINDOW_SECONDS = 15 * 60
MIB = 1024 ** 2
MAX_THRESHOLD_MIB = 1_000_000


@dataclass(frozen=True)
class AlertSettings:
    enabled: bool = False
    threshold_mib: int = 256

    def __post_init__(self):
        if not 1 <= self.threshold_mib <= MAX_THRESHOLD_MIB:
            raise ValueError("阈值超出范围")


@dataclass(frozen=True)
class SpikeAlert:
    start: float
    end: float
    total: int
    contributors: tuple[str | None, ...]
    peak_at: float


class SpikeDetector:
    def __init__(self, settings):
        self.settings = settings
        self.last_alert_at = None
        self.samples = deque()
        self.reset()

    def reset(self):
        self.samples.clear()
        self.start = None
        self.total = 0

    def add(self, start, end, apps):
        if not self.settings.enabled:
            return None
        if self.start is None:
            self.start = start
        amount = sum(up + down for up, down in apps.values())
        if amount:
            self.samples.append((start, end, apps, amount))
            self.total += amount
        cutoff = end - WINDOW_SECONDS
        # A sample crossing the start is ambiguous. Count only complete intervals.
        while self.samples and self.samples[0][0] < cutoff:
            _, _, _, expired = self.samples.popleft()
            self.total -= expired
        if self.start > cutoff or self.total < self.settings.threshold_mib * MIB:
            return None
        if self.last_alert_at is not None and 0 <= end - self.last_alert_at < WINDOW_SECONDS:
            return None
        self.last_alert_at = end
        totals = {}
        buckets = {}
        for _, timestamp, deltas, volume in self.samples:
            bucket = minute_bucket(timestamp)
            buckets[bucket] = buckets.get(bucket, 0) + volume
            for app, (up, down) in deltas.items():
                key = app.casefold() if app else ""
                name, previous = totals.get(key, (app, 0))
                totals[key] = (name, previous + up + down)
        ranked = sorted(totals.values(), key=lambda row: (-row[1], (row[0] or "").casefold()))
        peak = max(buckets, key=lambda bucket: (buckets[bucket], bucket))
        return SpikeAlert(cutoff, end, self.total, tuple(name for name, _ in ranked[:3]), peak)
