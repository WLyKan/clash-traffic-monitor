"""Verify a compiled Rust EXE using isolated D: data and an authenticated fixture.

The controller simulates cumulative proxy/direct/unknown connections; it does
not contact Mihomo or generate real external traffic. No process is terminated
by this script. Full verification prints READY_FOR_TRAY_EXIT and waits for the
owner to choose Exit in this isolated instance's tray menu.

Example:
    python tools/verify_rust_runtime.py --exe D:/.../clash-traffic-monitor.exe
Use --cli-only to verify read-only CLI modes without opening a desktop instance.
"""
import argparse
from contextlib import closing, contextmanager
from datetime import datetime, timezone
import hashlib
import http.server
import json
import os
from pathlib import Path
import secrets
import sqlite3
import subprocess
import sys
import threading
import time
import uuid

HOST = "127.0.0.1"
PROXY = (7_111, 15_013)
DIRECT = (123, 321)
UNKNOWN = (5, 11)
APP = "RuntimeProbe.exe"
TARGET = "example.invalid"
DEFAULT_ROOT = Path("D:/DevCache/ClashTrafficMonitorTauri")


def require(condition, message):
    if not condition:
        raise AssertionError(message)


def sha256(path):
    with path.open("rb") as source:
        return hashlib.file_digest(source, "sha256").hexdigest()


def emit(event, **values):
    print(json.dumps({"event": event, **values}, ensure_ascii=False), flush=True)


class FixtureController(http.server.ThreadingHTTPServer):
    daemon_threads = True

    def __init__(self):
        super().__init__((HOST, 0), FixtureHandler)
        self.token = secrets.token_urlsafe(32)
        self.counter = 0
        self.events = []
        self.mutex = threading.Lock()
        self.started = datetime.fromtimestamp(time.time()-60, timezone.utc).isoformat()

    def snapshot(self):
        with self.mutex:
            self.counter += 1
            sequence = self.counter
            self.events.append({"sequence": sequence, "at": time.time()})
        connections = []
        for identity, node, app, domain, pair in [
            ("proxy", "ProbeNode", APP, "EXAMPLE.INVALID.", PROXY),
            ("direct", "DIRECT", "DirectProbe.exe", "direct.invalid", DIRECT),
            ("unknown", "Selector", "UnknownProbe.exe", "unknown.invalid", UNKNOWN),
        ]:
            connections.append({
                "id": identity, "start": self.started,
                "upload": sequence*pair[0], "download": sequence*pair[1],
                "chains": [node],
                "metadata": {"processPath": f"C:\\Synthetic\\{app}", "host": domain},
            })
        return {
            "uploadTotal": sequence*sum(pair[0] for pair in (PROXY, DIRECT, UNKNOWN)),
            "downloadTotal": sequence*sum(pair[1] for pair in (PROXY, DIRECT, UNKNOWN)),
            "connections": connections,
        }


class FixtureHandler(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def do_GET(self):
        if self.headers.get("Authorization") != f"Bearer {self.server.token}":
            self.reply(401, {"message": "fixture authentication required"})
        elif self.path == "/proxies":
            self.reply(200, {"proxies": {
                "ProbeNode": {"type": "Trojan"}, "DIRECT": {"type": "Direct"},
                "Selector": {"type": "Selector"},
            }})
        elif self.path == "/connections":
            self.reply(200, self.server.snapshot())
        elif self.path == "/providers/proxies":
            self.reply(200, {"providers": {}})
        else:
            self.reply(404, {"message": "fixture route not found"})

    def reply(self, status, value):
        body = json.dumps(value, ensure_ascii=False).encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Type", "application/json; charset=utf-8")
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Connection", "close")
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *_):
        pass


@contextmanager
def controller():
    server = FixtureController()
    worker = threading.Thread(target=server.serve_forever, kwargs={"poll_interval": .05}, daemon=True)
    worker.start()
    try:
        yield server
    finally:
        server.shutdown()
        server.server_close()
        worker.join()


def readonly_state(path):
    uri = path.resolve().as_uri()+"?mode=ro"
    with closing(sqlite3.connect(uri, uri=True, timeout=2)) as db:
        db.execute("BEGIN")
        samples = db.execute("SELECT COALESCE(SUM(samples),0) FROM hours").fetchone()[0]
        checkpoint = db.execute("SELECT sampled_at FROM checkpoint WHERE id=1").fetchone()
        apps = db.execute("SELECT app,SUM(uploaded),SUM(downloaded) FROM apps GROUP BY app").fetchall()
        unknown = db.execute("SELECT SUM(unattributed_up),SUM(unattributed_down),SUM(unmatched_up),SUM(unmatched_down) FROM hours").fetchone()
        targets = db.execute("SELECT app,target,SUM(uploaded),SUM(downloaded) FROM targets GROUP BY app,target").fetchall()
        mismatch_hours = db.execute("""SELECT a.day,a.hour,a.app,a.uploaded,a.downloaded,
            COALESCE(SUM(t.uploaded),0),COALESCE(SUM(t.downloaded),0)
            FROM apps a LEFT JOIN targets t ON a.day=t.day AND a.hour=t.hour AND a.app=t.app
            GROUP BY a.day,a.hour,a.app HAVING a.uploaded!=COALESCE(SUM(t.uploaded),0)
                OR a.downloaded!=COALESCE(SUM(t.downloaded),0)""").fetchall()
        mismatch_minutes = db.execute("""SELECT a.bucket,a.app,a.uploaded,a.downloaded,
            COALESCE(SUM(t.uploaded),0),COALESCE(SUM(t.downloaded),0)
            FROM minute_apps a LEFT JOIN minute_targets t ON a.bucket=t.bucket AND a.app=t.app
            GROUP BY a.bucket,a.app HAVING a.uploaded!=COALESCE(SUM(t.uploaded),0)
                OR a.downloaded!=COALESCE(SUM(t.downloaded),0)""").fetchall()
        return {"samples": samples, "checkpoint": checkpoint[0] if checkpoint else None,
                "apps": apps, "targets": targets, "unattributed_and_residual": unknown,
                "hour_target_mismatches": mismatch_hours, "minute_target_mismatches": mismatch_minutes}


def assert_state(state):
    intervals = state["samples"]-1
    require(intervals >= 1, f"insufficient continuous samples: {state}")
    require(state["apps"] == [(APP, intervals*PROXY[0], intervals*PROXY[1])], f"proxy attribution or baseline mismatch: {state}")
    require(state["targets"] == [(APP, TARGET, intervals*PROXY[0], intervals*PROXY[1])], f"target attribution mismatch: {state}")
    require(state["unattributed_and_residual"] == (intervals*UNKNOWN[0], intervals*UNKNOWN[1], 0, 0), f"unknown route/residual mismatch: {state}")
    require(not state["hour_target_mismatches"], f"hour target conservation failed: {state}")
    require(not state["minute_target_mismatches"], f"minute target conservation failed: {state}")


def wait_for_samples(path, child, minimum, seconds=20):
    deadline = time.monotonic()+seconds
    last_error = None
    while time.monotonic() < deadline:
        require(child.poll() is None, f"collector exited early with {child.returncode}")
        if path.is_file():
            try:
                state = readonly_state(path)
                if state["samples"] >= minimum:
                    return state
            except sqlite3.Error as error:
                last_error = str(error)
        time.sleep(.1)
    raise TimeoutError(f"collector did not reach {minimum} samples: {last_error}")


def file_contents(directory):
    return {path.name: {"sha256": sha256(path), "bytes": path.stat().st_size}
            for path in directory.iterdir() if path.is_file()}


class Verification:
    def __init__(self, args):
        self.exe = args.exe.resolve(strict=True)
        self.directory = args.work_dir.resolve()
        require(os.name == "nt", "this verifier targets the Windows EXE")
        require(self.directory.drive.upper() == "D:", "all test data must be in a task-specific D: directory")
        require(not self.directory.exists(), "choose a new empty task directory; existing content is never reused")
        self.directory.mkdir(parents=True)
        self.report_path = self.directory/"verification.json"
        self.environment = os.environ.copy()
        # Even no-argument diagnostics must resolve to this isolated profile.
        self.environment["USERPROFILE"] = str(self.directory/"profile")
        self.environment["HOME"] = str(self.directory/"profile")
        self.report = {"exe": str(self.exe), "exe_sha256": sha256(self.exe),
                       "work_dir": str(self.directory), "fixture": "authenticated synthetic cumulative connections",
                       "cli": [], "phase": "starting", "manual_exit_required": False}
        self.collector = None
        self.logs = []
        self.args = args
        self.save()

    def save(self):
        self.report_path.write_text(json.dumps(self.report, ensure_ascii=False, indent=2)+"\n", encoding="utf-8")

    def cli(self, arguments, expected):
        child = subprocess.Popen([str(self.exe),*map(str,arguments)], cwd=self.directory,
                                 env=self.environment, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                 creationflags=subprocess.CREATE_NO_WINDOW)
        started = time.monotonic()
        try:
            stdout,stderr = child.communicate(timeout=20)
        except subprocess.TimeoutExpired:
            self.report.update(manual_exit_required=True, unexpected_live_pid=child.pid)
            self.save()
            raise TimeoutError(f"CLI PID {child.pid} did not exit; owner must close this confirmed test instance") from None
        result = {"arguments":list(map(str,arguments)), "pid":child.pid, "exit_code":child.returncode,
                  "seconds":round(time.monotonic()-started,3),
                  "stdout":stdout.decode("utf-8",errors="replace"), "stderr":stderr.decode("utf-8",errors="replace")}
        self.report["cli"].append(result)
        self.save()
        require(child.returncode == expected, f"unexpected CLI exit: {result}")
        return result

    def cli_modes(self):
        version=self.cli(["--version"],0)
        require("Clash 流量监控" in version["stdout"], f"version output missing application identity: {version}")
        for arguments in [["--data-dir"], ["--data-dir="],
                          ["--data-dir","--check"], ["--data-dir","--diagnostics"],
                          ["--unknown-probe-option"], ["--check","--diagnostics"]]:
            result=self.cli(arguments,1)
            require(result["stderr"].strip(), f"missing stderr for invalid arguments: {result}")
        for name in ("--check", "--diagnostics"):
            require(not (self.directory/name).exists(), f"CLI flag was misinterpreted as a data directory: {name}")
        absent=self.directory/"missing # % 数据"
        result=self.cli(["--diagnostics","--data-dir",absent],0)
        report=json.loads(result["stdout"])
        self.report["build_info"]={key:report.get(key) for key in ("runtime","version","build_commit","build_dirty","frontend_mode","embedded_index","embedded_script")}
        if not self.args.cli_only:
            require(report.get("frontend_mode") == "embedded" and report.get("embedded_index") is True
                    and report.get("embedded_script") is True,
                    "GUI acceptance requires embedded assets: use tauri build --debug --no-bundle or a release build; plain cargo build needs the Vite dev server")
        require(report["database_exists"] is False and not absent.exists(), "missing-directory diagnostics wrote data")
        require(Path(report["physical_data_dir"]) == absent, "custom data-dir physical resolution mismatch")
        result=self.cli(["--diagnostics"],0)
        default=self.directory/"profile"/"Documents"/"ClashTrafficMonitor"
        require(Path(json.loads(result["stdout"])["physical_data_dir"]) == default, "default data directory escaped isolated profile")
        require(not default.exists(), "default diagnostics created a data directory")
        legacy=self.directory/"legacy # % 数据"
        legacy.mkdir()
        with closing(sqlite3.connect(legacy/"traffic.sqlite3")) as db,db:
            db.executescript("""CREATE TABLE hours(day TEXT,hour INTEGER,samples INTEGER,gap_seconds REAL);
                CREATE TABLE apps(day TEXT,hour INTEGER,app TEXT,uploaded INTEGER,downloaded INTEGER);
                CREATE TABLE checkpoint(id INTEGER,sampled_at REAL);
                CREATE TABLE history_imports(source_key TEXT PRIMARY KEY,digest TEXT,imported_at REAL);
                INSERT INTO hours VALUES('2026-01-01',0,3,2.5);
                INSERT INTO apps VALUES('2026-01-01',0,'Legacy.exe',5,7);
                INSERT INTO checkpoint VALUES(1,12345.5);
                INSERT INTO history_imports VALUES('legacy-origin','preserved-digest',1.0);""")
        before=file_contents(legacy)
        result=self.cli(["--diagnostics",f"--data-dir={legacy}"],0)
        report=json.loads(result["stdout"])
        require(report.get("database_error") is None, f"legacy diagnostics failed: {report}")
        require(report["days"][0]["uploaded"] == 5 and report["days"][0]["downloaded"] == 7, "legacy totals changed")
        require(report["days"][0]["covered_seconds"] is None, "legacy unknown coverage was fabricated")
        require(file_contents(legacy) == before, "read-only diagnostics migrated or modified the legacy database")
        check_directory=self.directory/"check-only"
        result=self.cli(["--check","--data-dir",check_directory],0)
        require("代理 1 条" in result["stdout"] and "已识别应用的代理连接 1 条" in result["stdout"], f"check attribution mismatch: {result}")
        require(not check_directory.exists(), "--check opened or created a database")
        self.report["readonly_legacy_files"] = before

    def live_collector(self, server):
        directory=self.directory/"collector"
        arguments=[str(self.exe),"--start-hidden","--data-dir",str(directory)]
        stdout=(self.directory/"collector.stdout.log").open("wb")
        stderr=(self.directory/"collector.stderr.log").open("wb")
        self.logs.extend([stdout,stderr])
        first_sequence=server.counter
        self.collector=subprocess.Popen(arguments,cwd=self.directory,env=self.environment,stdout=stdout,stderr=stderr,
                                        creationflags=subprocess.CREATE_NO_WINDOW)
        self.report.update(collector_pid=self.collector.pid,collector_arguments=arguments,
                           collector_data_dir=str(directory),manual_exit_required=True,phase="sampling")
        self.save()
        database=directory/"traffic.sqlite3"
        first=wait_for_samples(database,self.collector,4)
        assert_state(first)
        duplicate=self.cli(arguments[1:],0)
        require(self.collector.poll() is None, "duplicate launch replaced the original process")
        later=wait_for_samples(database,self.collector,first["samples"]+3)
        assert_state(later)
        require(later["checkpoint"]>first["checkpoint"], "sampling checkpoint did not increase")
        self.report.update(first_state=first,later_state=later,duplicate_pid=duplicate["pid"],
                           phase="awaiting_tray_exit")
        self.save()
        emit("READY_FOR_TRAY_EXIT",pid=self.collector.pid,exe=str(self.exe),data_dir=str(directory),
             report=str(self.report_path),message="仅从此独立实例的托盘选择退出；验证脚本不会终止任何进程。")
        deadline=time.monotonic()+self.args.exit_timeout
        while self.collector.poll() is None and time.monotonic()<deadline:
            time.sleep(.2)
        require(self.collector.poll() is not None,
                f"tray exit pending for isolated PID {self.collector.pid}; process was left running")
        require(self.collector.returncode == 0,f"tray exit returned {self.collector.returncode}")
        final=readonly_state(database)
        assert_state(final)
        require(final["samples"] == server.counter-first_sequence,
                f"multiple readers/writers or uncommitted samples: SQL={final['samples']}, controller={server.counter-first_sequence}")
        with closing(sqlite3.connect(database)) as db:
            require(db.execute("PRAGMA quick_check").fetchall() == [("ok",)],"final database integrity failure")
            require(not db.execute("PRAGMA foreign_key_check").fetchall(),"final foreign-key failure")
        require(not (directory/"instance.json").exists(),"instance endpoint was not removed on tray exit")
        self.report.update(final_state=final,collector_exit_code=self.collector.returncode,
                           controller_sample_count=server.counter-first_sequence,manual_exit_required=False)

    def run(self):
        try:
            with controller() as server:
                self.environment["CLASH_MONITOR_CONTROLLER"]=f"http://{HOST}:{server.server_port}"
                self.environment["CLASH_MONITOR_SECRET"]=server.token
                self.cli_modes()
                if not self.args.cli_only:
                    self.live_collector(server)
                require(sha256(self.exe) == self.report["exe_sha256"],"EXE changed during verification")
            self.report["phase"]="passed"
            return 0
        except Exception as error:
            self.report.update(phase="failed",error=f"{type(error).__name__}: {error}")
            if self.collector is not None:
                self.report["manual_exit_required"]=self.collector.poll() is None
            return 1
        finally:
            for log in self.logs:
                log.close()
            self.save()
            emit("VERIFICATION_RESULT",phase=self.report["phase"],report=str(self.report_path),
                 manual_exit_required=self.report["manual_exit_required"],
                 collector_pid=self.report.get("collector_pid"),error=self.report.get("error"))


def main():
    sys.stdout.reconfigure(encoding="utf-8")
    sys.stderr.reconfigure(encoding="utf-8")
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--exe",required=True,type=Path)
    parser.add_argument("--work-dir",type=Path,default=DEFAULT_ROOT/f"runtime-probe-{uuid.uuid4().hex[:12]}")
    parser.add_argument("--cli-only",action="store_true")
    parser.add_argument("--exit-timeout",type=float,default=1200,
                        help="Seconds to wait for owner-controlled tray exit; never terminates the process")
    args=parser.parse_args()
    require(args.exit_timeout>0,"exit timeout must be positive")
    return Verification(args).run()


if __name__ == "__main__":
    sys.exit(main())
