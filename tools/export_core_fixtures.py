"""Freeze original Python behavior for the framework-independent Rust port.

Run from the repository root with Python. No database or live controller is used.
The committed result is consumed by monitor-core/tests/python_parity.rs.
"""
from dataclasses import asdict
import json
from pathlib import Path
import sys

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT))
from clash_monitor.collector import traffic_delta
from clash_monitor.source import Connection, Snapshot, target_name
from clash_monitor.alerts import AlertSettings, SpikeDetector, MIB


def conn(identity, up, down, start=0, route="proxy", app="Code.exe", target="目标未知"):
    return Connection(identity, start, app, route, up, down, target)


def delta_case(name, previous, current):
    targets = {}
    apps, unknown, unmatched = traffic_delta(previous, current, targets=targets)
    return dict(name=name, previous=asdict(previous), current=asdict(current), expected=dict(
        apps={key or "": value for key, value in apps.items()}, unknown_route=unknown, unmatched=unmatched,
        targets=sorted([[app or "", target, up, down] for (app, target), (up, down) in targets.items()])))


def export():
    cases = [
        delta_case("confirmed-proxy-direct-unknown-and-first-seen-baseline",
            Snapshot(100, 1000, 2000, (conn("old", 20, 30), conn("direct", 0, 0, route="direct"))),
            Snapshot(101, 1090, 2290, (conn("old", 40, 50), conn("direct", 30, 200, route="direct"),
                conn("unknown", 10, 10, 100.2, "unknown"), conn("no-app", 5, 5, 100.4, app=None),
                conn("missed", 500, 800)))),
        delta_case("request-overlap-first-seen-is-baseline", Snapshot(100, 10, 10, (), completed_at=100.1),
            Snapshot(101, 15, 15, (conn("new", 12, 12, 100.05),))),
        delta_case("new-connection-cumulative-and-ended-tail", Snapshot(100, 100, 100, (conn("ended", 40, 40),)),
            Snapshot(101, 130, 140, (conn("new", 20, 30, 100.2),))),
        delta_case("reused-id-new-start", Snapshot(100, 100, 100, (conn("same", 500, 500),)),
            Snapshot(101, 110, 120, (conn("same", 10, 20, 100.2),))),
        delta_case("non-atomic-signed-residual", Snapshot(100, 100, 100, (conn("same", 100, 100),)),
            Snapshot(101, 100, 200, (conn("same", 200, 100),))),
        delta_case("target-learned-and-counter-reset", Snapshot(100, 100, 100, (conn("same", 20, 20),)),
            Snapshot(101, 110, 120, (conn("same", 30, 10, target="example.com"),))),
        delta_case("hour-and-five-minute-boundary", Snapshot(1794345599, 20, 30, (conn("same", 20, 30),)),
            Snapshot(1794345600, 25, 39, (conn("same", 25, 39, target="2001:db8::1"),))),
    ]
    metadata = [
        {"host": " EXAMPLE.COM... "}, {"host": "127.0.0.1", "sniffHost": "Sniff.Example."},
        {"sniffHost": "only-sniff.example"}, {"destinationIP": "192.0.2.1"},
        {"destinationIP": "[2001:0DB8:0:0::1]"}, {}, {"host": "-bad", "sniffHost": "good.example"},
        {"host": "例子.测试"}, {"host": "faß.de"}, {"host": "a_b.example", "destinationIP": "::1"},
        {"host": "[::1]", "sniffHost": "[::2]", "destinationIP": "::ffff:192.0.2.1"},
        {"host": "a..b"}, {"host": "a" * 64 + ".com"}, {"host": "https://example.com"},
        {"host": "123"}, {"host": "foo\u200cbar.example"},
    ]
    detector = SpikeDetector(AlertSettings(True, 1))
    events, alerts = [], []
    t = 1794337200.0
    for second in range(1800):
        apps = {"Code.exe": (MIB, 0)} if second == 0 else ({"code.EXE": (0, 50), "other.exe": (0, 100)} if second == 899 else
            {"code.EXE": (MIB, 0)} if second == 900 else {None: (0, 1)} if second == 1799 else {})
        # Zero intervals are reconstructed, keeping the fixture compact.
        if apps:
            events.append(dict(second=second, apps={key or "": pair for key, pair in apps.items()}))
        alert = detector.add(t + second, t + second + 1, apps)
        if alert:
            row = asdict(alert)
            row["contributors"] = [name or "" for name in alert.contributors]
            alerts.append(row)
    return dict(version=1, deltas=cases, targets=[dict(metadata=row, expected=target_name(row)) for row in metadata],
        alerts=dict(start=t, seconds=1800, events=events, expected=alerts))


if __name__ == "__main__":
    path = ROOT / "tests/fixtures/rust-core/python-core.json"
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(export(), ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    print(path)
