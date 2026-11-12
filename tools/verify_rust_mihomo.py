"""Run the real installed Mihomo -> Rust source/sampling/SQLite acceptance.

Python is only a test driver and loopback traffic generator. The sole SQLite
writer is the explicitly selected Rust console probe. No desktop/production
collector or system proxy/DNS configuration is changed.
"""
import argparse
from contextlib import ExitStack, closing
import hashlib
import http.server
import json
import os
from pathlib import Path
import secrets
import shutil
import sqlite3
import subprocess
import sys
import threading
import time
import uuid

from verify_mihomo import (BODY_BYTES, CHUNK, DEFAULT_CORE, HOST, Origin, PROXY,
                           PYTHON_CLIENT, RESPONSE_BYTES, Relay, ThreadingTcpServer,
                           free_ports, process, read_api, running_server, wait_for_core)

ROOT = Path("D:/DevCache/ClashTrafficMonitorTauri")


def require(condition, message):
    if not condition:
        raise AssertionError(message)


def digest(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def database_state(path):
    with closing(sqlite3.connect(path.resolve().as_uri()+"?mode=ro", uri=True, timeout=2)) as db:
        db.execute("BEGIN")
        apps = db.execute("SELECT app,SUM(uploaded),SUM(downloaded) FROM apps GROUP BY app ORDER BY app").fetchall()
        return {"apps": apps, "samples": db.execute("SELECT SUM(samples) FROM hours").fetchone()[0],
                "checkpoint": db.execute("SELECT sampled_at FROM checkpoint WHERE id=1").fetchone()[0]}


class RustReader:
    def __init__(self, resources, exe, controller, token, work):
        self.events = []
        self.mutex = threading.Lock()
        self.database = work/"collector"/"traffic.sqlite3"
        self.log = resources.enter_context((work/"rust-events.jsonl").open("w", encoding="utf-8"))
        self.stderr = resources.enter_context((work/"rust.stderr.log").open("wb"))
        env = os.environ.copy()
        env["CLASH_MONITOR_SECRET"] = token
        env["TEMP"] = env["TMP"] = str(ROOT/"tmp")
        self.child = resources.enter_context(process([str(exe), "--controller", controller,
            "--data-dir", str(work/"collector"), "--seconds", "60"], cwd=work, env=env,
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=self.stderr))
        def read():
            for raw in self.child.stdout:
                line = raw.decode("utf-8")
                self.log.write(line)
                self.log.flush()
                event = json.loads(line)
                with self.mutex:
                    self.events.append(event)
        self.worker = threading.Thread(target=read, daemon=True)
        self.worker.start()
        resources.callback(self.close)
        self.wait(lambda rows: any(row.get("event") == "ready" for row in rows), "Rust ready")

    def close(self):
        # Own only the console helper spawned above. In failure cleanup stop it
        # before closing its logs; the production collector is never discovered
        # or selected by process name.
        if self.child.poll() is None:
            try:
                self.child.stdin.write(b"stop\n")
                self.child.stdin.flush()
                self.child.wait(10)
            except (OSError, subprocess.TimeoutExpired):
                self.child.terminate()
                self.child.wait(3)
        self.worker.join(3)

    def rows(self):
        with self.mutex:
            return list(self.events)

    def wait(self, predicate, label, timeout=12):
        deadline = time.monotonic()+timeout
        while time.monotonic() < deadline:
            rows = self.rows()
            require(not any(row.get("event") == "error" for row in rows), f"Rust probe error: {rows[-2:]}")
            if predicate(rows):
                return rows
            require(self.child.poll() is None, f"Rust reader exited while waiting for {label}: {self.child.returncode}")
            time.sleep(.05)
        raise TimeoutError(f"timed out waiting for {label}")

    def stop(self):
        self.child.stdin.write(b"stop\n")
        self.child.stdin.flush()
        self.child.wait(12)
        self.worker.join(3)
        require(self.child.returncode == 0, f"Rust probe exit {self.child.returncode}")
        rows = self.rows()
        require(rows[-1]["event"] == "finished", "Rust did not finish integrity/conservation checks")
        return rows[-1]


def observe_clients(api_port, token, clients, specs, fallback, evidence):
    histories = {name: [] for name in specs}
    deadline = time.monotonic()+18
    while time.monotonic() < deadline:
        snapshot = read_api(api_port, token, "/connections")
        evidence.write(json.dumps({"observed_at":time.time(),"snapshot":snapshot},ensure_ascii=False)+"\n")
        evidence.flush()
        for row in snapshot.get("connections") or []:
            meta = row["metadata"]
            for name, (expected, target, route) in specs.items():
                if str(meta.get("destinationPort")) != str(target):
                    continue
                # Concurrent proxy flows share a port, so always mode uses the
                # reported process; off mode keeps independent connection IDs
                # and the Rust reader must supply the actual process identity.
                reported = Path(meta.get("processPath") or meta.get("process") or "").name.casefold()
                if not fallback and reported != expected:
                    continue
                if fallback:
                    require(not meta.get("process") and not meta.get("processPath"), "Mihomo still supplied a process in fallback round")
                require(row.get("chains", [None])[0] == route, f"wrong real egress: {row}")
                histories[name].append((row["id"], row["upload"], row["download"]))
        if all(child.poll() is not None for child in clients):
            break
        time.sleep(.075)
    else:
        raise TimeoutError("real loopback traffic did not finish")
    for child in clients:
        _, error = child.communicate(timeout=2)
        require(child.returncode == 0, f"traffic client failed: {error.decode(errors='replace')}")
    return histories


def rust_flows(reader, specs):
    result = []
    for name, (expected, _port, route) in specs.items():
        normalized_route = "direct" if route == "DIRECT" else "proxy"
        by_id = {}
        for event in reader.rows():
            for row in event.get("snapshot", {}).get("connections", []):
                if (row.get("app") or "").casefold() == expected and row["route"] == normalized_route:
                    by_id.setdefault(row["id"], []).append((row["uploaded"], row["downloaded"]))
        require(len(by_id) == 1, f"{name}: expected exactly one Rust-attributed real connection, got {by_id.keys()}")
        identity, samples = next(iter(by_id.items()))
        require(len(samples) >= 2, f"{name}: insufficient Rust samples")
        for index, label in [(0, "upload"), (1, "download")]:
            counts = [pair[index] for pair in samples]
            require(counts == sorted(counts), f"{name}: {label} decreased")
            require(counts[-1]-counts[0] >= CHUNK, f"{name}: {label} did not grow by a chunk")
        result.append({"test": name, "process": expected, "route": normalized_route, "id": identity,
                       "samples": len(samples), "first": samples[0], "last": samples[-1]})
    return result


def run_round(exe, core_path, fallback):
    work = ROOT/f"real-mihomo-probe-{uuid.uuid4().hex[:12]}"
    work.mkdir(parents=True, exist_ok=False)
    report = {"work_dir": str(work), "mode": "off" if fallback else "always", "phase": "starting"}
    token = secrets.token_urlsafe(32)
    try:
        with ExitStack() as resources:
            proxied = resources.enter_context(running_server(http.server.ThreadingHTTPServer((HOST, 0), Origin)))
            direct = resources.enter_context(running_server(http.server.ThreadingHTTPServer((HOST, 0), Origin)))
            proxied.transfers = {}; direct.transfers = {}
            relay = resources.enter_context(running_server(ThreadingTcpServer((HOST, 0), Relay)))
            relay.origin_port = proxied.server_port; relay.accepted = []
            mixed, api_port = free_ports()
            config = work/"config.yaml"
            config.write_text(f"""mixed-port: {mixed}
allow-lan: false
bind-address: {HOST}
mode: rule
log-level: error
ipv6: false
geo-auto-update: false
find-process-mode: '{'off' if fallback else 'always'}'
external-controller: {HOST}:{api_port}
secret: {token}
tun:
  enable: false
dns:
  enable: false
proxies:
  - name: {PROXY}
    type: http
    server: {HOST}
    port: {relay.server_address[1]}
rules:
  - DST-PORT,{direct.server_port},DIRECT
  - DST-PORT,{proxied.server_port},{PROXY}
  - MATCH,REJECT
""", encoding="utf-8")
            core_log = resources.enter_context((work/"core.log").open("w+b"))
            core = resources.enter_context(process([str(core_path), "-d", str(work), "-f", str(config)],
                                                   cwd=work, stdout=core_log, stderr=subprocess.STDOUT))
            report["mihomo"] = wait_for_core(core, api_port, mixed, token)
            raw_evidence = resources.enter_context((work/"raw-mihomo.jsonl").open("w",encoding="utf-8"))
            reader = RustReader(resources, exe, f"http://{HOST}:{api_port}", token, work)
            reader.wait(lambda rows: sum(row.get("event") == "sample" for row in rows) >= 2, "baseline samples")
            initial = database_state(reader.database)
            require(not initial["apps"], "fresh Rust baseline contained apps")
            payload = work/"payload.bin"; payload.write_bytes(b"u"*BODY_BYTES)
            curl = shutil.which("curl.exe")
            require(curl, "curl.exe is required")
            curl_args = [curl, "--disable", "--silent", "--show-error", "--fail", "--max-time", "12",
                "--proxy", f"http://{HOST}:{mixed}", "--proxytunnel", "--noproxy", "", "--limit-rate", "64K",
                "--data-binary", f"@{payload}", "--output", "NUL"]
            direct_client = resources.enter_context(process(curl_args+[f"http://{HOST}:{direct.server_port}/direct"],
                stdout=subprocess.DEVNULL, stderr=subprocess.PIPE))
            direct_specs = {"curl-direct": (Path(curl).name.casefold(), direct.server_port, "DIRECT")}
            raw_direct = observe_clients(api_port, token, [direct_client], direct_specs, fallback, raw_evidence)
            time.sleep(.4)
            after_direct = database_state(reader.database)
            require(not after_direct["apps"], f"Rust incorrectly counted DIRECT as proxy: {after_direct}")
            require(after_direct["checkpoint"] > initial["checkpoint"], "Rust checkpoint did not advance during direct-only traffic")
            commands = [curl_args+[f"http://{HOST}:{proxied.server_port}/curl"],
                [sys.executable,"-I","-S","-B","-c",PYTHON_CLIENT,str(mixed),str(proxied.server_port),str(BODY_BYTES),str(RESPONSE_BYTES)]]
            clients = [resources.enter_context(process(command,stdout=subprocess.DEVNULL,stderr=subprocess.PIPE)) for command in commands]
            proxy_specs = {"curl-proxy": (Path(curl).name.casefold(),proxied.server_port,PROXY),
                           "python-proxy": (Path(sys.executable).name.casefold(),proxied.server_port,PROXY)}
            raw_proxy = observe_clients(api_port, token, clients, proxy_specs, fallback, raw_evidence)
            time.sleep(.4)
            flows = rust_flows(reader, {**direct_specs, **proxy_specs})
            # The independent real-controller observer and Rust parser must have
            # observed the same actual tracker IDs, including the fallback run.
            for flow in flows:
                raw = raw_direct if flow["route"] == "direct" else raw_proxy
                require(any(row[0] == flow["id"] for row in raw[flow["test"]]), f"Rust ID not observed in real Mihomo: {flow}")
            final = reader.stop()
            require({row["app"].casefold() for row in final["apps"]} == {Path(curl).name.casefold(),Path(sys.executable).name.casefold()}, "Rust did not persist both proxy applications")
            require(all(row["uploaded"] > 0 and row["downloaded"] > 0 for row in final["apps"]), "both proxy directions must increase")
            require(relay.accepted == [proxied.server_port]*2, f"relay accepted unexpected connections: {relay.accepted}")
            require(proxied.transfers == {"/curl":BODY_BYTES,"/python":BODY_BYTES}, f"proxy origin payload mismatch: {proxied.transfers}")
            require(direct.transfers == {"/direct":BODY_BYTES}, f"direct origin payload mismatch: {direct.transfers}")
            with closing(sqlite3.connect(reader.database.resolve().as_uri()+"?mode=ro",uri=True)) as db:
                targets = db.execute("SELECT DISTINCT target FROM targets").fetchall()
                require(targets == [(HOST,)], f"real IP targets were not normalized/persisted: {targets}")
            report.update(phase="passed", flows=flows, direct_only_state=after_direct, final=final,
                rust_pid=reader.child.pid,core_pid=core.pid,loopback_relay_connections=len(relay.accepted),
                request_body_bytes=BODY_BYTES,response_body_bytes=RESPONSE_BYTES, fallback_raw_process_fields_empty=fallback,
                raw_evidence=str(work/"raw-mihomo.jsonl"),rust_evidence=str(work/"rust-events.jsonl"))
        report["test_children_exited"] = True
    except Exception as error:
        report.update(phase="failed",error=f"{type(error).__name__}: {error}")
    finally:
        # Ephemeral credentials are not retained in the evidence config.
        if (work/"config.yaml").exists():
            text=(work/"config.yaml").read_text(encoding="utf-8").replace(token,"<redacted>")
            (work/"config.yaml").write_text(text,encoding="utf-8")
        (work/"report.json").write_text(json.dumps(report,ensure_ascii=False,indent=2)+"\n",encoding="utf-8")
    print(json.dumps(report,ensure_ascii=False),flush=True)
    return report


def main():
    sys.stdout.reconfigure(encoding="utf-8")
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--reader-exe",required=True,type=Path)
    parser.add_argument("--core",type=Path,default=DEFAULT_CORE)
    args=parser.parse_args()
    exe=args.reader_exe.resolve(strict=True);core=args.core.resolve(strict=True)
    require(os.name == "nt", "this acceptance targets native Windows process attribution")
    require(exe.name == "verify_mihomo_runtime.exe", "only the development console probe is accepted")
    ROOT.mkdir(parents=True,exist_ok=True)
    before=(digest(exe),digest(core))
    reports=[run_round(exe,core,fallback) for fallback in (False,True)]
    require(before == (digest(exe),digest(core)), "test binaries changed during acceptance")
    summary={"event":"real_mihomo_rust_acceptance","rust_exe":str(exe),"rust_sha256":before[0],
        "core":str(core),"core_sha256":before[1],"rounds":[str(Path(row["work_dir"])/"report.json") for row in reports],
        "passed":all(row["phase"]=="passed" for row in reports)}
    print(json.dumps(summary,ensure_ascii=False),flush=True)
    return 0 if summary["passed"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
