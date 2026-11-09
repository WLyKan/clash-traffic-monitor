"""Run a loopback-only attribution test with the installed Mihomo binary."""

import argparse
import contextlib
import http.client
import http.server
import json
import secrets
import select
import shutil
import socket
import socketserver
import subprocess
import sys
import tempfile
import threading
import time
from pathlib import Path


HOST = "127.0.0.1"
PROXY = "probe-relay"
BODY_BYTES = 96 * 1024
RESPONSE_BYTES = 128 * 1024
CHUNK = 8 * 1024
DEFAULT_CORE = Path(r"D:\Program Files\Clash Verge\verge-mihomo.exe")


class Origin(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def do_POST(self):
        uploaded = len(self.rfile.read(int(self.headers["Content-Length"])))
        self.server.transfers[self.path] = uploaded
        self.send_response(200)
        self.send_header("Content-Length", str(RESPONSE_BYTES))
        self.send_header("Connection", "close")
        self.end_headers()
        for _ in range(RESPONSE_BYTES // CHUNK):
            self.wfile.write(b"d" * CHUNK)
            self.wfile.flush()
            time.sleep(0.08)

    def log_message(self, *_):
        pass


class Relay(socketserver.StreamRequestHandler):
    def handle(self):
        self.connection.settimeout(5)
        request = self.rfile.readline(4096).split()
        allowed = f"{HOST}:{self.server.origin_port}".encode("ascii")
        if len(request) < 2 or request[0] != b"CONNECT" or request[1] != allowed:
            self.connection.sendall(b"HTTP/1.1 403 Forbidden\r\nConnection: close\r\n\r\n")
            return
        for line in self.rfile:
            if line == b"\r\n":
                break
        with socket.create_connection((HOST, self.server.origin_port), timeout=5) as origin:
            self.server.accepted.append(self.server.origin_port)
            self.connection.sendall(b"HTTP/1.1 200 Connection Established\r\n\r\n")
            links = {self.connection: origin, origin: self.connection}
            while True:
                readable, _, _ = select.select(list(links), [], [], 5)
                if not readable:
                    return
                for source in readable:
                    data = source.recv(65536)
                    if not data:
                        return
                    links[source].sendall(data)


class ThreadingTcpServer(socketserver.ThreadingTCPServer):
    daemon_threads = True


@contextlib.contextmanager
def running_server(server):
    worker = threading.Thread(target=server.serve_forever, kwargs={"poll_interval": 0.05}, daemon=True)
    worker.start()
    try:
        yield server
    finally:
        server.shutdown()
        server.server_close()
        worker.join()


def free_ports():
    with socket.socket() as one, socket.socket() as two:
        one.bind((HOST, 0))
        two.bind((HOST, 0))
        return one.getsockname()[1], two.getsockname()[1]


def read_api(port, token, path):
    conn = http.client.HTTPConnection(HOST, port, timeout=1)
    try:
        conn.request("GET", path, headers={"Authorization": f"Bearer {token}"})
        response = conn.getresponse()
        if response.status != 200:
            raise RuntimeError(f"{path}: HTTP {response.status}")
        return json.loads(response.read())
    finally:
        conn.close()


def wait_for_core(core, port, mixed, token):
    for _ in range(80):
        if core.poll() is not None:
            raise RuntimeError(f"Mihomo exited with code {core.returncode}")
        try:
            version = read_api(port, token, "/version")["version"]
            with socket.create_connection((HOST, mixed), timeout=0.2):
                return version
        except OSError:
            time.sleep(0.075)
    raise RuntimeError("Mihomo control API and mixed listener did not become ready")


@contextlib.contextmanager
def process(command, **kwargs):
    child = subprocess.Popen(command, creationflags=subprocess.CREATE_NO_WINDOW, **kwargs)
    try:
        yield child
    finally:
        if child.poll() is None:
            child.terminate()
            try:
                child.wait(3)
            except subprocess.TimeoutExpired:
                child.kill()
                child.wait()


PYTHON_CLIENT = """
import http.client
import sys
import time

mixed, target, body_bytes, response_bytes = map(int, sys.argv[1:])
conn = http.client.HTTPConnection('127.0.0.1', mixed, timeout=8)
conn.set_tunnel('127.0.0.1', target)
conn.putrequest('POST', '/python')
conn.putheader('Content-Length', str(body_bytes))
conn.endheaders()
for _ in range(body_bytes // 8192):
    conn.send(b'u' * 8192)
    time.sleep(.08)
response = conn.getresponse()
if response.status != 200 or len(response.read()) != response_bytes:
    sys.exit('unexpected response status or body length')
conn.close()
"""


def observe(api_port, token, children, specs):
    histories = {name: [] for name in specs}
    deadline = time.monotonic() + 16
    while time.monotonic() < deadline:
        snapshot = read_api(api_port, token, "/connections")
        for row in snapshot.get("connections") or []:
            meta = row["metadata"]
            exe = Path(meta.get("processPath") or "").name.casefold()
            reported = meta.get("process", "").casefold()
            for name, (expected, target, route) in specs.items():
                if str(meta.get("destinationPort")) != str(target) or exe != expected:
                    continue
                assert reported == expected, (name, reported)
                assert row["chains"] and row["chains"][0] == route, (name, row["chains"])
                histories[name].append((row["id"], row["upload"], row["download"]))
        if all(child.poll() is not None for child in children):
            break
        time.sleep(0.075)
    else:
        raise RuntimeError("Controlled clients did not finish within 16 seconds")
    for child in children:
        _, stderr = child.communicate(timeout=2)
        if child.returncode:
            raise RuntimeError(f"Controlled client exited {child.returncode}: {stderr.decode(errors='replace')}")

    result = []
    for name, samples in histories.items():
        expected, _, route = specs[name]
        assert len(samples) >= 2, f"{name}: insufficient application-attributed samples: {len(samples)}"
        assert len({s[0] for s in samples}) == 1, f"{name}: multiple trackers"
        uploads = [s[1] for s in samples]
        downloads = [s[2] for s in samples]
        for label, counter in (("upload", uploads), ("download", downloads)):
            assert counter == sorted(counter), f"{name}: {label} counter decreased"
            assert counter[-1] - counter[0] >= CHUNK, f"{name}: {label} did not measurably increase"
        result.append({
            "test": name,
            "process": expected,
            "egress": route,
            "samples": len(samples),
            "upload_first": uploads[0],
            "upload_last": uploads[-1],
            "download_first": downloads[0],
            "download_last": downloads[-1],
        })
    assert len({samples[0][0] for samples in histories.values()}) == len(specs)
    return result


def verify(core_path):
    curl = shutil.which("curl.exe")
    if not curl or not core_path.is_file():
        raise RuntimeError("curl.exe and the installed Mihomo core are required")
    token = secrets.token_urlsafe(32)
    with contextlib.ExitStack() as resources:
        work = Path(__file__).resolve().parent
        home = Path(resources.enter_context(tempfile.TemporaryDirectory(prefix="mihomo-probe-", dir=work)))
        proxied = resources.enter_context(running_server(http.server.ThreadingHTTPServer((HOST, 0), Origin)))
        direct = resources.enter_context(running_server(http.server.ThreadingHTTPServer((HOST, 0), Origin)))
        proxied.transfers = {}
        direct.transfers = {}
        proxy_target, direct_target = proxied.server_port, direct.server_port
        relay = resources.enter_context(running_server(ThreadingTcpServer((HOST, 0), Relay)))
        relay.origin_port = proxy_target
        relay.accepted = []
        mixed, api_port = free_ports()
        config = home / "config.yaml"
        config.write_text(f"""mixed-port: {mixed}
allow-lan: false
bind-address: {HOST}
mode: rule
log-level: error
ipv6: false
geo-auto-update: false
find-process-mode: always
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
  - DST-PORT,{direct_target},DIRECT
  - DST-PORT,{proxy_target},{PROXY}
  - MATCH,REJECT
""", encoding="utf-8")
        payload = home / "payload.bin"
        payload.write_bytes(b"u" * BODY_BYTES)
        core_log = resources.enter_context((home / "core.log").open("w+b"))
        core = resources.enter_context(process([str(core_path), "-d", str(home), "-f", str(config)],
                                               stdout=core_log, stderr=subprocess.STDOUT))
        try:
            version = wait_for_core(core, api_port, mixed, token)
        except RuntimeError as error:
            core_log.seek(0)
            diagnostic = core_log.read().decode(errors="replace").replace(token, "<redacted>")
            raise RuntimeError(f"{error}\n{diagnostic}") from None
        types = read_api(api_port, token, "/proxies")["proxies"]
        assert types[PROXY]["type"].lower() == "http", types[PROXY]["type"]
        assert types["DIRECT"]["type"].lower() == "direct", types["DIRECT"]["type"]
        curl_args = [curl, "--disable", "--silent", "--show-error", "--fail", "--max-time", "12",
                     "--proxy", f"http://{HOST}:{mixed}", "--proxytunnel", "--noproxy", "",
                     "--limit-rate", "64K", "--data-binary", f"@{payload}", "--output", "NUL"]
        commands = [
            curl_args + [f"http://{HOST}:{proxy_target}/curl"],
            [sys.executable, "-I", "-S", "-B", "-c", PYTHON_CLIENT,
             str(mixed), str(proxy_target), str(BODY_BYTES), str(RESPONSE_BYTES)],
            curl_args + [f"http://{HOST}:{direct_target}/direct"],
        ]
        clients = [resources.enter_context(process(command, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE))
                   for command in commands]
        specs = {
            "curl-proxy": (Path(curl).name.casefold(), proxy_target, PROXY),
            "python-proxy": (Path(sys.executable).name.casefold(), proxy_target, PROXY),
            "curl-direct": (Path(curl).name.casefold(), direct_target, "DIRECT"),
        }
        samples = observe(api_port, token, clients, specs)
        assert relay.accepted == [proxy_target, proxy_target], relay.accepted
        assert proxied.transfers == {"/curl": BODY_BYTES, "/python": BODY_BYTES}, proxied.transfers
        assert direct.transfers == {"/direct": BODY_BYTES}, direct.transfers
        return {"mihomo": version, "loopback_relay_connections": len(relay.accepted), "flows": samples}


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--core", type=Path, default=DEFAULT_CORE)
    print(json.dumps(verify(parser.parse_args().core.resolve()), indent=2, ensure_ascii=False))
