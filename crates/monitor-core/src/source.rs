//! Read-only Mihomo transport, route classification and process attribution.
use crate::{time::unix_now, Connection, Route, Snapshot};
use serde_json::Value;
use std::{
    collections::{HashMap, HashSet},
    io::{Read, Write},
    net::{IpAddr, SocketAddr, TcpStream},
    time::{Duration, Instant},
};

#[cfg(windows)]
mod windows;

const MAX_BODY: usize = 32 * 1024 * 1024;
const TIMEOUT: Duration = Duration::from_secs(2);
const ROUTE_CACHE: Duration = Duration::from_secs(10);
const INVALID: &str = "Mihomo 控制接口返回了无效数据。";

pub fn target_name(metadata: &Value) -> String {
    for field in ["host", "sniffHost"] {
        let Some(input) = metadata.get(field).and_then(Value::as_str) else {
            continue;
        };
        let value = input.trim().trim_end_matches('.').to_lowercase();
        if value.trim_matches(['[', ']']).parse::<IpAddr>().is_ok() {
            continue;
        }
        #[allow(deprecated)]
        let Ok(ascii) = idna::Config::default()
            .transitional_processing(true)
            .use_std3_ascii_rules(false)
            .verify_dns_length(false)
            .to_ascii(&value)
        else {
            continue;
        };
        if ascii.len() <= 253
            && ascii.split('.').all(|label| {
                !label.is_empty()
                    && label.len() <= 63
                    && !label.starts_with('-')
                    && !label.ends_with('-')
                    && label
                        .bytes()
                        .all(|c| c.is_ascii_alphanumeric() || c == b'-')
            })
        {
            return ascii;
        }
    }
    metadata
        .get("destinationIP")
        .and_then(Value::as_str)
        .and_then(|s| s.trim_matches(['[', ']']).parse::<IpAddr>().ok())
        .map(|address| address.to_string())
        .unwrap_or_else(|| "目标未知".into())
}

pub fn route_type(name: &str) -> Route {
    match name.to_lowercase().as_str() {
        "direct" => Route::Direct,
        "shadowsocks" | "shadowsocksr" | "snell" | "socks5" | "http" | "vmess" | "vless"
        | "trojan" | "hysteria" | "hysteria2" | "wireguard" | "tuic" | "ssh" | "mieru"
        | "anytls" | "sudoku" | "masque" | "trusttunnel" | "shadowquic" | "openvpn"
        | "tailscale" | "zerotier" | "easytier" | "gostrelay" => Route::Proxy,
        _ => Route::Unknown,
    }
}

pub fn app_name(value: &str) -> Option<String> {
    let name = value.trim().rsplit(['\\', '/']).next().unwrap_or_default();
    (!name.is_empty() && name != "." && name != ".." && !name.chars().any(|c| (c as u32) < 32))
        .then(|| name.into())
}

fn first_name<'a>(row: &'a Value, key: &str) -> &'a str {
    row.get(key)
        .and_then(Value::as_array)
        .and_then(|items| items.first())
        .and_then(Value::as_str)
        .unwrap_or("")
}
fn start_time(value: &Value) -> Option<f64> {
    chrono::DateTime::parse_from_rfc3339(value.as_str()?)
        .ok()
        .map(|time| time.timestamp_micros() as f64 / 1_000_000.0)
}
fn byte_count(value: &Value) -> Option<i64> {
    value.as_i64().filter(|v| *v >= 0)
}

/// Injectable transport lets the exact parser/state machine run with frozen
/// controller fixtures while live data stays on the user's local controller.
pub trait Transport: Send {
    fn get(&mut self, path: &str) -> Result<Value, String>;
    fn server_pid(&self) -> Option<u32> {
        None
    }
}

pub struct HttpEndpoint {
    address: SocketAddr,
    authorization: String,
}
impl HttpEndpoint {
    pub fn new(controller: &str, secret: &str) -> Result<Self, String> {
        let input = if controller.contains("://") {
            controller.into()
        } else {
            format!("http://{controller}")
        };
        let bad_address = || "控制地址必须是带端口的本机 HTTP 地址。".to_string();
        let url = url::Url::parse(&input).map_err(|_| bad_address())?;
        let host = url.host_str().unwrap_or_default().trim_matches(['[', ']']);
        let host = if host == "localhost" {
            "127.0.0.1"
        } else {
            host
        };
        let address = host.parse::<IpAddr>().map_err(|_| bad_address())?;
        // Url normalizes an explicit :80 away, so inspect the authority too.
        let authority = input
            .split("://")
            .nth(1)
            .unwrap_or_default()
            .split('/')
            .next()
            .unwrap_or_default();
        let explicit_port = authority
            .rsplit_once(':')
            .and_then(|(_, p)| p.parse::<u16>().ok())
            .filter(|p| *p != 0);
        if url.scheme() != "http"
            || !url.username().is_empty()
            || url.password().is_some()
            || !matches!(url.path(), "" | "/")
            || url.query().is_some()
            || url.fragment().is_some()
            || !address.is_loopback()
            || explicit_port.is_none()
            || host.contains('%')
        {
            return Err(bad_address());
        }
        if secret.trim().is_empty() || !secret.bytes().all(|byte| (32..=126).contains(&byte)) {
            return Err("本机 HTTP 控制器需要有效的访问密钥。".into());
        }
        Ok(Self {
            address: SocketAddr::new(address, explicit_port.unwrap()),
            authorization: format!("Bearer {secret}"),
        })
    }
}
impl Transport for HttpEndpoint {
    fn get(&mut self, path: &str) -> Result<Value, String> {
        let stream = TcpStream::connect_timeout(&self.address, TIMEOUT).map_err(safe_io)?;
        let mut stream = ControllerStream::Tcp(stream);
        let request = format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nAuthorization: {}\r\nConnection: close\r\n\r\n", self.authorization);
        exchange(&mut stream, request.as_bytes())
    }
}

#[cfg(windows)]
struct PipeEndpoint {
    path: String,
    pid: Option<u32>,
}
#[cfg(windows)]
impl Transport for PipeEndpoint {
    fn get(&mut self, path: &str) -> Result<Value, String> {
        let stream = windows::Pipe::open(&self.path).map_err(safe_io)?;
        self.pid = stream.server_pid();
        let mut stream = ControllerStream::Pipe(stream);
        exchange(
            &mut stream,
            format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
                .as_bytes(),
        )
    }
    fn server_pid(&self) -> Option<u32> {
        self.pid
    }
}

fn safe_io(error: std::io::Error) -> String {
    if matches!(
        error.kind(),
        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
    ) {
        "读取 Mihomo 控制接口超时。".into()
    } else {
        "无法连接 Mihomo 控制接口。".into()
    }
}
enum ControllerStream {
    Tcp(TcpStream),
    #[cfg(windows)]
    Pipe(windows::Pipe),
}
impl ControllerStream {
    fn read(&mut self, buffer: &mut [u8], deadline: Instant) -> std::io::Result<usize> {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .ok_or(std::io::ErrorKind::TimedOut)?;
        match self {
            Self::Tcp(stream) => {
                stream.set_read_timeout(Some(remaining.max(Duration::from_millis(1))))?;
                stream.read(buffer)
            }
            #[cfg(windows)]
            Self::Pipe(stream) => stream.read(buffer, deadline),
        }
    }
    fn write_all(&mut self, mut buffer: &[u8], deadline: Instant) -> std::io::Result<()> {
        while !buffer.is_empty() {
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .ok_or(std::io::ErrorKind::TimedOut)?;
            let count = match self {
                Self::Tcp(stream) => {
                    stream.set_write_timeout(Some(remaining.max(Duration::from_millis(1))))?;
                    stream.write(buffer)?
                }
                #[cfg(windows)]
                Self::Pipe(stream) => stream.write(buffer, deadline)?,
            };
            if count == 0 {
                return Err(std::io::ErrorKind::WriteZero.into());
            }
            buffer = &buffer[count..];
        }
        Ok(())
    }
}

fn exchange(stream: &mut ControllerStream, request: &[u8]) -> Result<Value, String> {
    let deadline = Instant::now() + TIMEOUT;
    stream.write_all(request, deadline).map_err(safe_io)?;
    let mut buffer = Vec::new();
    let mut scratch = [0u8; 8192];
    let header_end = loop {
        if let Some(index) = buffer.windows(4).position(|s| s == b"\r\n\r\n") {
            break index + 4;
        }
        if buffer.len() > 65536 {
            return Err(INVALID.into());
        }
        let count = stream.read(&mut scratch, deadline).map_err(safe_io)?;
        if count == 0 {
            return Err(INVALID.into());
        }
        buffer.extend_from_slice(&scratch[..count]);
    };
    let headers = std::str::from_utf8(&buffer[..header_end]).map_err(|_| INVALID)?;
    let status = headers
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|v| v.parse::<u16>().ok())
        .ok_or(INVALID)?;
    match status {
        200 => {}
        401 | 403 => return Err("Mihomo 控制接口拒绝认证，请核对控制器密钥。".into()),
        _ => return Err(format!("Mihomo 控制接口返回 HTTP {status}。")),
    }
    let mut length = None;
    let mut chunked = false;
    for line in headers.lines().skip(1) {
        if let Some((key, value)) = line.split_once(':') {
            if key.eq_ignore_ascii_case("content-length") {
                length = Some(value.trim().parse::<usize>().map_err(|_| INVALID)?);
            }
            if key.eq_ignore_ascii_case("transfer-encoding") {
                chunked = value.trim().eq_ignore_ascii_case("chunked");
            }
        }
    }
    if length.is_some_and(|len| len > MAX_BODY) {
        return Err("Mihomo 控制接口响应过大。".into());
    }
    let mut body = buffer[header_end..].to_vec();
    loop {
        let completed = if chunked {
            decode_chunks(&body)?
        } else if let Some(length) = length {
            (body.len() >= length).then(|| body[..length].to_vec())
        } else {
            None
        };
        if let Some(body) = completed {
            return parse_json(&body);
        }
        if body.len() > MAX_BODY + 65536 {
            return Err("Mihomo 控制接口响应过大。".into());
        }
        let count = stream.read(&mut scratch, deadline).map_err(safe_io)?;
        if count == 0 {
            return if chunked || length.is_some() {
                Err(INVALID.into())
            } else {
                parse_json(&body)
            };
        }
        body.extend_from_slice(&scratch[..count]);
    }
}
fn parse_json(body: &[u8]) -> Result<Value, String> {
    if body.len() > MAX_BODY {
        return Err("Mihomo 控制接口响应过大。".into());
    }
    let value: Value = serde_json::from_slice(body).map_err(|_| INVALID)?;
    if !value.is_object() {
        return Err(INVALID.into());
    }
    Ok(value)
}
fn decode_chunks(input: &[u8]) -> Result<Option<Vec<u8>>, String> {
    let mut offset = 0;
    let mut result = Vec::new();
    loop {
        let Some(end) = input[offset..]
            .windows(2)
            .position(|v| v == b"\r\n")
            .map(|i| i + offset)
        else {
            return Ok(None);
        };
        let size = std::str::from_utf8(&input[offset..end])
            .ok()
            .and_then(|s| {
                usize::from_str_radix(s.split(';').next().unwrap_or_default().trim(), 16).ok()
            })
            .ok_or(INVALID)?;
        offset = end + 2;
        if size == 0 {
            return Ok((input.len() >= offset + 2).then_some(result));
        }
        if result
            .len()
            .checked_add(size)
            .is_none_or(|len| len > MAX_BODY)
        {
            return Err("Mihomo 控制接口响应过大。".into());
        }
        if input.len() < offset + size + 2 {
            return Ok(None);
        }
        if input[offset + size..offset + size + 2] != *b"\r\n" {
            return Err(INVALID.into());
        }
        result.extend_from_slice(&input[offset..offset + size]);
        offset += size + 2;
    }
}

pub struct MihomoSource {
    endpoints: Vec<Box<dyn Transport>>,
    active: Option<usize>,
    active_pid: Option<u32>,
    generation: u64,
    routes: HashMap<String, Route>,
    routes_at: Option<Instant>,
    routes_pid: Option<u32>,
    provider_routes: HashMap<(String, String), Route>,
    provider_names: HashMap<String, Route>,
    providers_at: Option<Instant>,
    apps: HashMap<String, String>,
}
impl MihomoSource {
    pub fn from_env() -> Result<Self, String> {
        Self::new(
            std::env::var("CLASH_MONITOR_CONTROLLER").ok().as_deref(),
            std::env::var("CLASH_MONITOR_SECRET").ok().as_deref(),
        )
    }
    pub fn new(controller: Option<&str>, secret: Option<&str>) -> Result<Self, String> {
        let endpoints: Vec<Box<dyn Transport>> = if let Some(controller) = controller {
            vec![Box::new(HttpEndpoint::new(
                controller,
                secret.unwrap_or_default(),
            )?)]
        } else {
            #[cfg(windows)]
            {
                windows::default_pipes()?
                    .into_iter()
                    .map(|path| Box::new(PipeEndpoint { path, pid: None }) as Box<dyn Transport>)
                    .collect()
            }
            #[cfg(not(windows))]
            {
                return Err("此平台请配置带密钥的本机 HTTP 控制器。".into());
            }
        };
        Ok(Self::with_transports(endpoints))
    }
    pub fn with_transports(endpoints: Vec<Box<dyn Transport>>) -> Self {
        Self {
            endpoints,
            active: None,
            active_pid: None,
            generation: 0,
            routes: HashMap::new(),
            routes_at: None,
            routes_pid: None,
            provider_routes: HashMap::new(),
            provider_names: HashMap::new(),
            providers_at: None,
            apps: HashMap::new(),
        }
    }
    pub fn sample(&mut self) -> Result<Snapshot, String> {
        let mut indexes: Vec<usize> = (0..self.endpoints.len()).collect();
        indexes.sort_by_key(|index| Some(*index) != self.active);
        let mut last = "无法读取 Clash Verge 的本地控制管道。请确认 Clash Verge 正在运行，且程序由同一 Windows 用户启动。".to_string();
        for index in indexes {
            match self.sample_endpoint(index) {
                Ok(snapshot) => return Ok(snapshot),
                Err(error) => {
                    self.routes_at = None;
                    last = error;
                }
            }
        }
        Err(last)
    }
    fn sample_endpoint(&mut self, index: usize) -> Result<Snapshot, String> {
        if self.active != Some(index) {
            self.routes_at = None;
        }
        if self.routes_at.is_none_or(|at| at.elapsed() >= ROUTE_CACHE) {
            let payload = self.endpoints[index].get("/proxies")?;
            let proxies = payload
                .get("proxies")
                .and_then(Value::as_object)
                .ok_or("Mihomo 代理列表格式无效。")?;
            self.routes = proxies
                .iter()
                .filter(|(_, v)| v.is_object())
                .map(|(name, value)| {
                    (
                        name.clone(),
                        route_type(
                            value
                                .get("type")
                                .and_then(Value::as_str)
                                .unwrap_or_default(),
                        ),
                    )
                })
                .collect();
            self.routes_pid = self.endpoints[index].server_pid();
            self.routes_at = Some(Instant::now());
        }
        let sampled_at = unix_now();
        let payload = self.endpoints[index].get("/connections")?;
        let completed_at = unix_now();
        let upload_total =
            byte_count(&payload["uploadTotal"]).ok_or("Mihomo 流量快照格式无效。")?;
        let download_total =
            byte_count(&payload["downloadTotal"]).ok_or("Mihomo 流量快照格式无效。")?;
        let rows = match payload.get("connections") {
            None | Some(Value::Null) => &[][..],
            Some(Value::Array(rows)) => rows.as_slice(),
            _ => return Err("Mihomo 流量快照格式无效。".into()),
        };
        let pid = self.endpoints[index].server_pid();
        let changed = self.active != Some(index);
        let pid_changed = pid.is_some() && self.active_pid.is_some() && pid != self.active_pid;
        if self.active.is_some() && (changed || pid_changed) {
            self.generation += 1;
            self.apps.clear();
            self.provider_routes.clear();
            self.provider_names.clear();
            self.providers_at = None;
        }
        if pid.is_some() && self.routes_pid.is_some() && pid != self.routes_pid {
            self.routes.clear();
            self.routes_at = None;
        }
        if changed || pid.is_some() {
            self.active_pid = pid;
        }
        self.active = Some(index);
        if self
            .providers_at
            .is_none_or(|at| at.elapsed() >= ROUTE_CACHE)
            && rows.iter().any(|row| {
                let leaf = first_name(row, "chains");
                !leaf.is_empty()
                    && (!first_name(row, "providerChains").is_empty()
                        || !self.routes.contains_key(leaf))
            })
        {
            self.refresh_providers(index);
        }
        let connections = self.connections(rows);
        Ok(Snapshot {
            sampled_at,
            completed_at: Some(completed_at),
            upload_total,
            download_total,
            generation: self.generation,
            connections,
        })
    }
    fn refresh_providers(&mut self, index: usize) {
        self.providers_at = Some(Instant::now());
        self.provider_routes.clear();
        self.provider_names.clear();
        let Ok(payload) = self.endpoints[index].get("/providers/proxies") else {
            return;
        };
        if self.active_pid.is_some()
            && self.endpoints[index].server_pid().is_some()
            && self.active_pid != self.endpoints[index].server_pid()
        {
            return;
        }
        let Some(providers) = payload.get("providers").and_then(Value::as_object) else {
            return;
        };
        for (provider, data) in providers {
            let Some(proxies) = data.get("proxies").and_then(Value::as_array) else {
                continue;
            };
            for node in proxies {
                let Some(name) = node
                    .get("name")
                    .and_then(Value::as_str)
                    .filter(|name| !name.is_empty())
                else {
                    continue;
                };
                let route =
                    route_type(node.get("type").and_then(Value::as_str).unwrap_or_default());
                let entry = self
                    .provider_routes
                    .entry((provider.clone(), name.into()))
                    .or_insert(route);
                if *entry != route {
                    *entry = Route::Unknown;
                }
                let entry = self.provider_names.entry(name.into()).or_insert(route);
                if *entry != route {
                    *entry = Route::Unknown;
                }
            }
        }
    }
    pub fn connection_route(&self, row: &Value) -> Route {
        let leaf = first_name(row, "chains");
        let provider = first_name(row, "providerChains");
        if !provider.is_empty() {
            self.provider_routes
                .get(&(provider.into(), leaf.into()))
                .copied()
                .unwrap_or_default()
        } else {
            self.routes
                .get(leaf)
                .or_else(|| self.provider_names.get(leaf))
                .copied()
                .unwrap_or_default()
        }
    }
    fn connections(&mut self, rows: &[Value]) -> Vec<Connection> {
        self.connections_with(rows, tcp_owners, process_details)
    }
    fn connections_with(
        &mut self,
        rows: &[Value],
        owners: impl FnOnce(&HashSet<SocketKey>) -> HashMap<SocketKey, Option<u32>>,
        mut details: impl FnMut(u32) -> Option<(String, f64)>,
    ) -> Vec<Connection> {
        let mut seen = HashSet::new();
        let mut pending = Vec::new();
        let mut needed = HashSet::new();
        for row in rows {
            let Some(id) = row
                .get("id")
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty())
            else {
                continue;
            };
            let (Some(uploaded), Some(downloaded)) =
                (byte_count(&row["upload"]), byte_count(&row["download"]))
            else {
                continue;
            };
            if !seen.insert(id) {
                continue;
            }
            let metadata = &row["metadata"];
            let app = metadata
                .get("processPath")
                .and_then(Value::as_str)
                .and_then(app_name)
                .or_else(|| {
                    metadata
                        .get("process")
                        .and_then(Value::as_str)
                        .and_then(app_name)
                })
                .or_else(|| self.apps.get(id).cloned());
            let socket = app.is_none().then(|| connection_socket(metadata)).flatten();
            if let Some(socket) = socket {
                needed.insert(socket);
            }
            pending.push((
                Connection {
                    id: id.into(),
                    started_at: start_time(&row["start"]),
                    app,
                    route: self.connection_route(row),
                    uploaded,
                    downloaded,
                    target: target_name(metadata),
                },
                socket,
            ));
        }
        let owners = owners(&needed);
        let mut processes = HashMap::new();
        let result: Vec<_> = pending
            .into_iter()
            .map(|(mut record, socket)| {
                if record.app.is_none() {
                    if let Some(pid) =
                        socket.and_then(|socket| owners.get(&socket).copied().flatten())
                    {
                        if let Some((name, created)) =
                            processes.entry(pid).or_insert_with(|| details(pid))
                        {
                            if record.started_at.is_none_or(|start| *created <= start) {
                                record.app = Some(name.clone());
                            }
                        }
                    }
                }
                record
            })
            .collect();
        self.apps = result
            .iter()
            .filter_map(|record| {
                record
                    .app
                    .as_ref()
                    .map(|app| (record.id.clone(), app.clone()))
            })
            .collect();
        result
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SocketKey {
    pub local: SocketAddr,
    pub peer: SocketAddr,
}
pub fn normalize_address(address: &str, port: u16) -> Option<SocketAddr> {
    if port == 0 {
        return None;
    }
    let address: IpAddr = address.parse().ok()?;
    let address = match address {
        IpAddr::V6(v6) => v6
            .to_ipv4_mapped()
            .map(IpAddr::V4)
            .unwrap_or(IpAddr::V6(v6)),
        address => address,
    };
    Some(SocketAddr::new(address, port))
}
pub fn connection_socket(metadata: &Value) -> Option<SocketKey> {
    if !metadata
        .get("network")?
        .as_str()?
        .eq_ignore_ascii_case("tcp")
    {
        return None;
    }
    let port = |key: &str| -> Option<u16> {
        let value = metadata.get(key)?;
        value
            .as_u64()
            .and_then(|v| u16::try_from(v).ok())
            .or_else(|| value.as_str()?.parse().ok())
    };
    Some(SocketKey {
        local: normalize_address(metadata.get("sourceIP")?.as_str()?, port("sourcePort")?)?,
        peer: normalize_address(metadata.get("inboundIP")?.as_str()?, port("inboundPort")?)?,
    })
}
fn tcp_owners(needed: &HashSet<SocketKey>) -> HashMap<SocketKey, Option<u32>> {
    #[cfg(windows)]
    {
        windows::tcp_owners(needed)
    }
    #[cfg(not(windows))]
    {
        let _ = needed;
        HashMap::new()
    }
}
fn process_details(pid: u32) -> Option<(String, f64)> {
    #[cfg(windows)]
    {
        windows::process_details(pid)
    }
    #[cfg(not(windows))]
    {
        let _ = pid;
        None
    }
}

#[cfg(test)]
mod tests;
