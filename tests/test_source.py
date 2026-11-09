from __future__ import annotations

import json
import os
import threading
import time
import unittest
import uuid
from contextlib import contextmanager
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from types import SimpleNamespace
from unittest.mock import patch

import psutil

from clash_monitor import source


def _row(identity, leaf, metadata):
    return {
        "id": identity, "start": "2026-11-10T01:00:00.123456789+08:00",
        "metadata": metadata, "chains": [leaf, "出口组"],
        "upload": 200, "download": 1200,
    }


@contextmanager
def _http_server():
    seen = []

    class Handler(BaseHTTPRequestHandler):
        def do_GET(self):
            seen.append((self.path, self.headers.get("Authorization")))
            if self.headers.get("Authorization") != "Bearer local-test-key":
                self.send_error(401)
                return
            data = {
                "/proxies": {"proxies": {
                    "台湾": {"type": "Trojan"}, "例外": {"type": "Direct"},
                    "出口组": {"type": "Selector"},
                }},
                "/connections": {"uploadTotal": 600, "downloadTotal": 3600, "connections": [
                    _row("proxy", "台湾", {"processPath": r"C:\Program Files\Test\App.EXE"}),
                    _row("direct", "例外", {"process": "browser.exe"}),
                    _row("unknown", "出口组", {}),
                ]},
            }[self.path]
            body = json.dumps(data, ensure_ascii=False).encode("utf-8")
            self.send_response(200)
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def log_message(self, format, *args):
            pass

    with ThreadingHTTPServer(("127.0.0.1", 0), Handler) as server:
        thread = threading.Thread(target=server.serve_forever, kwargs={"poll_interval": 0.02})
        thread.start()
        try:
            yield f"127.0.0.1:{server.server_port}", seen
        finally:
            server.shutdown()
            thread.join()


class HttpSourceTests(unittest.TestCase):
    def test_authorized_reads_use_leaf_type_and_keep_only_exe_name(self):
        with _http_server() as (controller, requests):
            client = source.MihomoSource(controller, "local-test-key")
            snapshot = client.sample()
            client.sample()
        self.assertEqual(requests, [
            ("/proxies", "Bearer local-test-key"),
            ("/connections", "Bearer local-test-key"),
            ("/connections", "Bearer local-test-key"),
        ])
        self.assertEqual((snapshot.upload_total, snapshot.download_total), (600, 3600))
        self.assertEqual([(c.app, c.route) for c in snapshot.connections], [
            ("App.EXE", "proxy"), ("browser.exe", "direct"), (None, "unknown"),
        ])
        self.assertIsNotNone(snapshot.connections[0].started_at)

    def test_reject_remote_controller_unsafe_secret_and_bad_auth(self):
        for controller in ("http://192.0.2.1:9090", "https://127.0.0.1:9090",
                           "http://127.0.0.1:9090/path", "http://["):
            with self.subTest(controller=controller), self.assertRaises(source.SourceError):
                source.MihomoSource(controller, "local-test-key")
        with self.assertRaises(source.SourceError):
            source.MihomoSource("127.0.0.1:9090", "header\r\ninjection")
        with _http_server() as (controller, _):
            with self.assertRaisesRegex(source.SourceError, "拒绝认证") as rejected:
                source.MihomoSource(controller, "wrong-secret").sample()
        self.assertNotIn("wrong-secret", str(rejected.exception))

    def test_provider_qualified_nodes_and_unknown_on_unavailable_endpoint(self):
        a = {**_row("a", "同名节点", {}), "providerChains": ["供应商甲", ""]}
        b = {**_row("b", "同名节点", {}), "providerChains": ["供应商乙", ""]}
        legacy = _row("old", "同名节点", {})
        connections = {"uploadTotal": 600, "downloadTotal": 3600, "connections": [a, b, legacy]}
        providers = {"providers": {
            "供应商甲": {"proxies": [{"name": "同名节点", "type": "Shadowsocks"}]},
            "供应商乙": {"proxies": [{"name": "同名节点", "type": "Direct"}]},
        }}
        with patch.object(source._HttpEndpoint, "get", side_effect=[
            {"proxies": {}}, connections, providers, connections
        ]) as get:
            client = source.MihomoSource("127.0.0.1:9090", "local-test-key")
            self.assertEqual([c.route for c in client.sample().connections], ["proxy", "direct", "unknown"])
            self.assertEqual([c.route for c in client.sample().connections], ["proxy", "direct", "unknown"])
        self.assertEqual([call.args[0] for call in get.call_args_list],
                         ["/proxies", "/connections", "/providers/proxies", "/connections"])
        with patch.object(source._HttpEndpoint, "get", side_effect=[
            {"proxies": {}}, connections, source.SourceError("unavailable")
        ]):
            client = source.MihomoSource("127.0.0.1:9090", "local-test-key")
            self.assertEqual([c.route for c in client.sample().connections], ["unknown"] * 3)

    def test_strict_fallback_uses_complete_four_tuple_and_caches_identity(self):
        metadata = {
            "network": "tcp", "sourceIP": "::ffff:127.0.0.1", "sourcePort": "49172",
            "inboundIP": "127.0.0.1", "inboundPort": "7897",
        }
        def entry(pid, remote):
            return SimpleNamespace(
                status=psutil.CONN_ESTABLISHED, laddr=("127.0.0.1", 49172),
                raddr=("127.0.0.1", remote), pid=pid,
            )
        client = source.MihomoSource()
        rows = [_row("a", "台湾", metadata)]
        with patch.object(psutil, "net_connections", return_value=[entry(42, 7897), entry(13, 7900)]) as owners:
            with patch.object(source, "_process_details", return_value=("curl.exe", 100)) as process:
                first = client._connections(rows)
                second = client._connections(rows)
                process.assert_called_once_with(42)
            owners.assert_called_once_with(kind="tcp")
        self.assertEqual(first[0].app, "curl.exe")
        self.assertEqual(second[0].app, "curl.exe")

        with patch.object(psutil, "net_connections", return_value=[entry(42, 7897), entry(13, 7897)]):
            result = client._connections([_row("b", "台湾", metadata)])
        self.assertIsNone(result[0].app)

        with patch.object(psutil, "net_connections", return_value=[entry(42, 7897)]):
            with patch.object(source, "_process_details", return_value=("new-owner.exe", time.time() + 86400 * 365)):
                reused = client._connections([_row("c", "台湾", metadata)])
        self.assertIsNone(reused[0].app)


@unittest.skipUnless(os.name == "nt", "Windows named pipe integration")
class NamedPipeSourceTests(unittest.TestCase):
    @contextmanager
    def pipe_server(self, response):
        import _winapi

        path = rf"\\.\pipe\clash-monitor-test-{uuid.uuid4().hex}"
        handle = _winapi.CreateNamedPipe(
            path, _winapi.PIPE_ACCESS_DUPLEX, _winapi.PIPE_WAIT, 1, 8192, 8192, 1000, 0
        )
        release = threading.Event()
        requests = []
        errors = []

        def run():
            try:
                try:
                    _winapi.ConnectNamedPipe(handle)
                except OSError as error:
                    if error.winerror != 535:  # ERROR_PIPE_CONNECTED
                        raise
                request, _ = _winapi.ReadFile(handle, 1024)
                requests.append(request)
                if response is not None:
                    _winapi.WriteFile(handle, response)
                release.wait(4)
            except Exception as error:
                errors.append(error)
            finally:
                _winapi.CloseHandle(handle)

        thread = threading.Thread(target=run, daemon=True)
        thread.start()
        try:
            yield source._PipeEndpoint(path), requests
        finally:
            release.set()
            thread.join(timeout=5)
            self.assertFalse(thread.is_alive(), "pipe test server did not terminate")
            self.assertEqual(errors, [])

    def test_chunked_utf8_response(self):
        body = '{"测试":"成功"}'.encode("utf-8")
        raw = (
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n"
            + f"{len(body):x}\r\n".encode("ascii") + body + b"\r\n0\r\n\r\n"
        )
        with self.pipe_server(raw) as (endpoint, requests):
            self.assertEqual(endpoint.get("/connections"), {"测试": "成功"})
            self.assertEqual(endpoint.server_pid, os.getpid())
        self.assertEqual(len(requests), 1)
        self.assertTrue(requests[0].startswith(b"GET /connections HTTP/1.1\r\n"))

    def test_read_deadline(self):
        with self.pipe_server(None) as (endpoint, requests), patch.object(source, "_HTTP_TIMEOUT", 0.25):
            start = time.monotonic()
            with self.assertRaises(TimeoutError):
                endpoint.get("/connections")
            self.assertLess(time.monotonic() - start, 1.5)
        self.assertEqual(len(requests), 1)

    def test_new_pipe_server_pid_changes_generation_even_when_counters_grow(self):
        endpoint = source._PipeEndpoint("test")
        pid = 100

        def get(path):
            endpoint.server_pid = pid
            if path == "/proxies":
                return {"proxies": {"Leaf": {"type": "Trojan"}}}
            if path == "/providers/proxies":
                return {"providers": {}}
            return {"uploadTotal": pid, "downloadTotal": pid, "connections": [_row("x", "Leaf", {})]}

        with patch.object(source, "_default_endpoints", return_value=(endpoint,)):
            with patch.object(endpoint, "get", side_effect=get):
                client = source.MihomoSource()
                before = client.sample()
                pid = 200
                after = client.sample()
                settled = client.sample()
        self.assertEqual((before.generation, after.generation, settled.generation), (0, 1, 1))
        self.assertEqual([s.connections[0].route for s in (before, after, settled)], ["proxy", "unknown", "proxy"])


if __name__ == "__main__":
    unittest.main()
