from __future__ import annotations

import hashlib
import http.client
import io
import ipaddress
import json
import ntpath
import os
import re
import subprocess
import time
from dataclasses import dataclass, field
from datetime import datetime
from urllib.parse import urlsplit

import psutil

if os.name == "nt":
    import ctypes
    import _winapi
    from ctypes import wintypes

    _get_pipe_pid = ctypes.WinDLL("kernel32").GetNamedPipeServerProcessId
    _get_pipe_pid.argtypes = [wintypes.HANDLE, ctypes.POINTER(wintypes.ULONG)]
    _get_pipe_pid.restype = wintypes.BOOL


_HTTP_TIMEOUT = 2.0
_MAX_BODY = 32 * 1024 * 1024
_ROUTE_CACHE_SECONDS = 10.0

# Concrete outbound adapter types in Mihomo 1.19.32.
_PROXY_TYPES = frozenset({
    "shadowsocks", "shadowsocksr", "snell", "socks5", "http", "vmess", "vless",
    "trojan", "hysteria", "hysteria2", "wireguard", "tuic", "ssh", "mieru",
    "anytls", "sudoku", "masque", "trusttunnel", "shadowquic", "openvpn",
    "tailscale", "zerotier", "easytier", "gostrelay",
})


class SourceError(RuntimeError):
    """A localized, safe-to-display source error."""


@dataclass(frozen=True)
class Connection:
    id: str
    started_at: float | None
    app: str | None
    route: str
    uploaded: int
    downloaded: int


@dataclass(frozen=True)
class Snapshot:
    sampled_at: float
    upload_total: int
    download_total: int
    connections: tuple[Connection, ...]
    generation: int = 0
    completed_at: float | None = None


def _read_json(response: http.client.HTTPResponse) -> dict:
    if response.status in (401, 403):
        raise SourceError("Mihomo 控制接口拒绝认证，请核对控制器密钥。")
    if response.status != 200:
        raise SourceError(f"Mihomo 控制接口返回 HTTP {response.status}。")
    body = response.read(_MAX_BODY + 1)
    if len(body) > _MAX_BODY:
        raise SourceError("Mihomo 控制接口响应过大。")
    try:
        payload = json.loads(body)
    except (ValueError, RecursionError):
        raise SourceError("Mihomo 控制接口返回了无效数据。") from None
    if not isinstance(payload, dict):
        raise SourceError("Mihomo 控制接口返回了无效数据。")
    return payload


class _HttpEndpoint:
    def __init__(self, controller: str, secret: str | None):
        try:
            url = urlsplit(controller if "://" in controller else f"http://{controller}")
            host = url.hostname
            address = ipaddress.ip_address("127.0.0.1" if host == "localhost" else host or "")
            port = url.port
        except ValueError:
            raise SourceError("控制地址必须是带端口的本机 HTTP 地址。") from None
        if (
            url.scheme != "http" or url.username is not None or url.password is not None
            or url.path not in ("", "/") or url.query or url.fragment
            or not address.is_loopback or not port or "%" in str(address)
        ):
            raise SourceError("控制地址必须是带端口的本机 HTTP 地址。")
        if not secret or not secret.strip() or any(not 32 <= ord(ch) <= 126 for ch in secret):
            raise SourceError("本机 HTTP 控制器需要有效的访问密钥。")
        self._host = str(address)
        self._port = port
        self._authorization = f"Bearer {secret}"

    def get(self, path: str) -> dict:
        connection = http.client.HTTPConnection(self._host, self._port, timeout=_HTTP_TIMEOUT)
        try:
            connection.request("GET", path, headers={
                "Authorization": self._authorization, "Connection": "close"
            })
            with connection.getresponse() as response:
                return _read_json(response)
        finally:
            connection.close()


def _wait_io(operation, status: int, deadline: float) -> tuple[int, int]:
    if status == _winapi.ERROR_IO_PENDING:
        remaining = max(0, int((deadline - time.monotonic()) * 1000))
        if _winapi.WaitForSingleObject(operation.event, remaining) == _winapi.WAIT_TIMEOUT:
            operation.cancel()
            # Drain the cancelled operation before releasing its native buffer.
            operation.GetOverlappedResult(True)
            raise TimeoutError("named pipe")
    return operation.GetOverlappedResult(True)


class _PipeStream(io.RawIOBase):
    """Adapt a Windows overlapped pipe to HTTPResponse's buffered binary reader."""

    def __init__(self, pipe: str):
        self._handle = None
        self._deadline = time.monotonic() + _HTTP_TIMEOUT
        # Identification level prevents the pipe server from impersonating the client.
        security_identification = 0x00110000
        self._handle = _winapi.CreateFile(
            pipe, _winapi.GENERIC_READ | _winapi.GENERIC_WRITE, 0, 0,
            _winapi.OPEN_EXISTING, _winapi.FILE_FLAG_OVERLAPPED | security_identification, 0,
        )

    def readable(self) -> bool:
        return True

    def server_pid(self) -> int | None:
        pid = wintypes.ULONG()
        return pid.value if _get_pipe_pid(self._handle, ctypes.byref(pid)) and pid.value else None

    def makefile(self, mode: str) -> io.BufferedReader:
        return io.BufferedReader(self)

    def close(self) -> None:
        try:
            if self._handle is not None:
                _winapi.CloseHandle(self._handle)
                self._handle = None
        finally:
            super().close()

    def send(self, data: bytes) -> None:
        operation, status = _winapi.WriteFile(self._handle, data, True)
        written, status = _wait_io(operation, status, self._deadline)
        if status or written != len(data):
            raise OSError("named pipe write")

    def readinto(self, buffer) -> int:
        if time.monotonic() >= self._deadline:
            raise TimeoutError("named pipe")
        try:
            operation, status = _winapi.ReadFile(self._handle, len(buffer), True)
            count, status = _wait_io(operation, status, self._deadline)
        except OSError as error:
            if getattr(error, "winerror", None) == _winapi.ERROR_BROKEN_PIPE:
                return 0
            raise
        if status == _winapi.ERROR_BROKEN_PIPE:
            return 0
        if status not in (0, _winapi.ERROR_MORE_DATA):
            raise OSError("named pipe read")
        buffer[:count] = operation.getbuffer()[:count]
        return count


@dataclass
class _PipeEndpoint:
    path: str
    server_pid: int | None = field(default=None, init=False, compare=False, repr=False)

    def get(self, path: str) -> dict:
        request = f"GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n"
        with _PipeStream(self.path) as stream:
            self.server_pid = stream.server_pid()
            stream.send(request.encode("ascii"))
            with http.client.HTTPResponse(stream) as response:
                response.begin()
                return _read_json(response)


def _default_endpoints() -> tuple[_PipeEndpoint, ...]:
    if os.name != "nt":
        raise SourceError("Clash Verge 命名管道仅在 Windows 上可用。")
    whoami = ntpath.join(os.environ.get("SystemRoot", r"C:\Windows"), "System32", "whoami.exe")
    try:
        identity = subprocess.run(
            [whoami, "/user", "/fo", "csv", "/nh"], capture_output=True,
            check=True, timeout=_HTTP_TIMEOUT, creationflags=subprocess.CREATE_NO_WINDOW,
        )
    except (OSError, subprocess.SubprocessError):
        raise SourceError("无法获取当前 Windows 用户的身份。") from None
    sid = re.search(rb"\bS-1-(?:\d+-)+\d+\b", identity.stdout)
    if sid is None:
        raise SourceError("无法获取当前 Windows 用户的身份。")
    owner = hashlib.sha256(sid.group()).hexdigest()
    return tuple(_PipeEndpoint(r"\\.\pipe" + "\\" + prefix + owner) for prefix in (
        "verge-mihomo-production-", "verge-mihomo-sidecar-release-",
    ))


def _byte_count(value: object) -> int | None:
    return value if type(value) is int and 0 <= value <= 2**63 - 1 else None


def _start_time(value: object) -> float | None:
    if not isinstance(value, str):
        return None
    try:
        started = datetime.fromisoformat(value)
        return started.timestamp() if started.tzinfo is not None else None
    except (ValueError, OverflowError, OSError):
        return None


def _app_name(value: object) -> str | None:
    if not isinstance(value, str):
        return None
    name = ntpath.basename(value.strip())
    if not name or name in (".", "..") or any(ord(ch) < 32 for ch in name):
        return None
    return name


def _socket_address(address: object, port: object) -> tuple[str, int] | None:
    if not isinstance(address, str) or type(port) not in (int, str):
        return None
    try:
        ip = ipaddress.ip_address(address)
        number = int(port)
    except ValueError:
        return None
    if not 1 <= number <= 65535:
        return None
    if isinstance(ip, ipaddress.IPv6Address) and ip.ipv4_mapped:
        ip = ip.ipv4_mapped
    return str(ip), number


def _connection_socket(metadata: dict) -> tuple | None:
    if str(metadata.get("network", "")).casefold() != "tcp":
        return None
    local = _socket_address(metadata.get("sourceIP"), metadata.get("sourcePort"))
    peer = _socket_address(metadata.get("inboundIP"), metadata.get("inboundPort"))
    return (local, peer) if local is not None and peer is not None else None


def _tcp_owners(sockets: set[tuple]) -> dict[tuple, int | None]:
    owners = {}
    if not sockets:
        return owners
    try:
        entries = psutil.net_connections(kind="tcp")
    except (psutil.Error, OSError):
        return owners
    for entry in entries:
        if entry.status != psutil.CONN_ESTABLISHED or not (entry.laddr and entry.raddr and entry.pid):
            continue
        local = _socket_address(*entry.laddr)
        peer = _socket_address(*entry.raddr)
        key = local, peer
        if key in sockets:
            owners[key] = entry.pid if key not in owners or owners[key] == entry.pid else None
    return owners


def _process_details(pid: int) -> tuple[str | None, float] | None:
    try:
        process = psutil.Process(pid)
        with process.oneshot():
            started = process.create_time()
            name = _app_name(process.name())
        return (name, started) if process.is_running() else None
    except (psutil.Error, OSError):
        return None


def _route_type(adapter_type: object) -> str:
    if isinstance(adapter_type, str):
        name = adapter_type.casefold()
        if name == "direct":
            return "direct"
        if name in _PROXY_TYPES:
            return "proxy"
    return "unknown"


def _first_name(row: dict, key: str) -> str:
    chain = row.get(key)
    return chain[0] if isinstance(chain, list) and chain and isinstance(chain[0], str) else ""


class MihomoSource:
    """Read-only Mihomo snapshots with optional Windows TCP owner lookup."""

    def __init__(self, controller: str | None = None, secret: str | None = None):
        self._http = _HttpEndpoint(controller, secret) if controller is not None else None
        self._pipes: tuple[_PipeEndpoint, ...] = ()
        self._active: _HttpEndpoint | _PipeEndpoint | None = None
        self._active_pid: int | None = None
        self._generation = 0
        self._routes: dict[str, str] = {}
        self._routes_at = float("-inf")
        self._routes_pid: int | None = None
        self._provider_routes: dict[tuple[str, str], str] = {}
        self._provider_routes_by_name: dict[str, str] = {}
        self._provider_routes_at = float("-inf")
        self._apps: dict[str, str] = {}

    def sample(self) -> Snapshot:
        if self._http is not None:
            return self._sample_checked(self._http)
        if not self._pipes:
            self._pipes = _default_endpoints()
        endpoints = sorted(self._pipes, key=lambda endpoint: endpoint != self._active)
        for endpoint in endpoints:
            try:
                return self._sample_checked(endpoint)
            except SourceError:
                continue
        raise SourceError(
            "无法读取 Clash Verge 的本地控制管道。请确认 Clash Verge 正在运行，"
            "且流量监控与它由同一 Windows 用户启动。"
        )

    def _sample_checked(self, endpoint: _HttpEndpoint | _PipeEndpoint) -> Snapshot:
        try:
            return self._sample(endpoint)
        except TimeoutError:
            self._routes_at = float("-inf")
            raise SourceError("读取 Mihomo 控制接口超时。") from None
        except (OSError, http.client.HTTPException):
            self._routes_at = float("-inf")
            raise SourceError("无法连接 Mihomo 控制接口。") from None
        except SourceError:
            self._routes_at = float("-inf")
            raise

    def _sample(self, endpoint: _HttpEndpoint | _PipeEndpoint) -> Snapshot:
        if endpoint != self._active:
            self._routes_at = float("-inf")
        if time.monotonic() - self._routes_at >= _ROUTE_CACHE_SECONDS:
            proxies = endpoint.get("/proxies").get("proxies")
            if not isinstance(proxies, dict):
                raise SourceError("Mihomo 代理列表格式无效。")
            self._routes = {
                name: _route_type(proxy.get("type"))
                for name, proxy in proxies.items() if isinstance(proxy, dict)
            }
            self._routes_pid = getattr(endpoint, "server_pid", None)
            self._routes_at = time.monotonic()

        sampled_at = time.time()
        payload = endpoint.get("/connections")
        completed_at = time.time()
        upload = _byte_count(payload.get("uploadTotal"))
        download = _byte_count(payload.get("downloadTotal"))
        rows = payload.get("connections")
        if rows is None:
            rows = []
        if upload is None or download is None or not isinstance(rows, list):
            raise SourceError("Mihomo 流量快照格式无效。")

        server_pid = getattr(endpoint, "server_pid", None)
        endpoint_changed = endpoint != self._active
        pid_changed = (
            server_pid is not None and self._active_pid is not None and server_pid != self._active_pid
        )
        if self._active is not None and (endpoint_changed or pid_changed):
            self._generation += 1
            self._apps.clear()
            self._provider_routes.clear()
            self._provider_routes_by_name.clear()
            self._provider_routes_at = float("-inf")
        if server_pid is not None and self._routes_pid is not None and server_pid != self._routes_pid:
            self._routes.clear()
            self._routes_at = float("-inf")
        if endpoint_changed or server_pid is not None:
            self._active_pid = server_pid
        self._active = endpoint
        if time.monotonic() - self._provider_routes_at >= _ROUTE_CACHE_SECONDS:
            for row in rows:
                if not isinstance(row, dict):
                    continue
                leaf = _first_name(row, "chains")
                if leaf and (_first_name(row, "providerChains") or leaf not in self._routes):
                    self._refresh_provider_routes(endpoint, server_pid)
                    break
        records = self._connections(rows)
        return Snapshot(sampled_at, upload, download, records, self._generation, completed_at)

    def _refresh_provider_routes(self, endpoint: _HttpEndpoint | _PipeEndpoint, server_pid: int | None) -> None:
        self._provider_routes_at = time.monotonic()
        self._provider_routes.clear()
        self._provider_routes_by_name.clear()
        try:
            providers = endpoint.get("/providers/proxies").get("providers")
        except (SourceError, OSError, http.client.HTTPException):
            return
        provider_pid = getattr(endpoint, "server_pid", None)
        if server_pid is not None and provider_pid is not None and server_pid != provider_pid:
            return
        if not isinstance(providers, dict):
            return
        for provider_name, provider in providers.items():
            nodes = provider.get("proxies") if isinstance(provider, dict) else None
            if not isinstance(nodes, list):
                continue
            for node in nodes:
                name = node.get("name") if isinstance(node, dict) else None
                if not isinstance(name, str) or not name:
                    continue
                route = _route_type(node.get("type"))
                key = provider_name, name
                if self._provider_routes.setdefault(key, route) != route:
                    self._provider_routes[key] = "unknown"
                # Older cores may omit providerChains. A conflicting name stays unknown.
                if self._provider_routes_by_name.setdefault(name, route) != route:
                    self._provider_routes_by_name[name] = "unknown"

    def _connection_route(self, row: dict) -> str:
        leaf = _first_name(row, "chains")
        provider = _first_name(row, "providerChains")
        if provider:
            return self._provider_routes.get((provider, leaf), "unknown")
        return self._routes.get(leaf, self._provider_routes_by_name.get(leaf, "unknown"))

    def _connections(self, rows: list) -> tuple[Connection, ...]:
        pending = []
        needed = set()
        seen_ids = set()
        for row in rows:
            if not isinstance(row, dict):
                continue
            identity = row.get("id")
            uploaded = _byte_count(row.get("upload"))
            downloaded = _byte_count(row.get("download"))
            if not isinstance(identity, str) or not identity or identity in seen_ids:
                continue
            if uploaded is None or downloaded is None:
                continue
            seen_ids.add(identity)
            metadata = row.get("metadata")
            if not isinstance(metadata, dict):
                metadata = {}
            started_at = _start_time(row.get("start"))
            route = self._connection_route(row)
            app = _app_name(metadata.get("processPath")) or _app_name(metadata.get("process"))
            app = app or self._apps.get(identity)
            socket = _connection_socket(metadata) if app is None else None
            if socket is not None:
                needed.add(socket)
            pending.append((identity, started_at, app, socket, route, uploaded, downloaded))

        owners = _tcp_owners(needed)
        processes = {}
        result = []
        for identity, started_at, app, socket, route, uploaded, downloaded in pending:
            pid = owners.get(socket)
            if app is None and pid is not None:
                if pid not in processes:
                    processes[pid] = _process_details(pid)
                details = processes[pid]
                if details is not None and (started_at is None or details[1] <= started_at):
                    app = details[0]
            result.append(Connection(identity, started_at, app, route, uploaded, downloaded))
        self._apps = {record.id: record.app for record in result if record.app is not None}
        return tuple(result)
