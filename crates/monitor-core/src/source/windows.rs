//! Narrow Windows adapters. Handles never escape this module.
use super::{app_name, SocketKey};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    ffi::c_void,
    io,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    ptr,
    time::Instant,
};
type Handle = *mut c_void;
#[repr(C)]
struct Overlapped {
    internal: usize,
    internal_high: usize,
    offset: u32,
    offset_high: u32,
    event: Handle,
}
#[repr(C)]
#[derive(Default)]
struct FileTime {
    low: u32,
    high: u32,
}
#[repr(C)]
#[derive(Clone, Copy)]
struct Tcp4 {
    state: u32,
    local: u32,
    local_port: u32,
    remote: u32,
    remote_port: u32,
    pid: u32,
}
#[repr(C)]
#[derive(Clone, Copy)]
struct Tcp6 {
    local: [u8; 16],
    local_scope: u32,
    local_port: u32,
    remote: [u8; 16],
    remote_scope: u32,
    remote_port: u32,
    state: u32,
    pid: u32,
}
#[link(name = "kernel32")]
extern "system" {
    fn CreateFileW(
        name: *const u16,
        access: u32,
        share: u32,
        security: *const c_void,
        disposition: u32,
        flags: u32,
        template: Handle,
    ) -> Handle;
    fn CloseHandle(handle: Handle) -> i32;
    fn CreateEventW(security: *const c_void, manual: i32, initial: i32, name: *const u16)
        -> Handle;
    fn ReadFile(
        handle: Handle,
        buffer: *mut c_void,
        length: u32,
        count: *mut u32,
        overlapped: *mut Overlapped,
    ) -> i32;
    fn WriteFile(
        handle: Handle,
        buffer: *const c_void,
        length: u32,
        count: *mut u32,
        overlapped: *mut Overlapped,
    ) -> i32;
    fn WaitForSingleObject(handle: Handle, timeout: u32) -> u32;
    fn GetOverlappedResult(
        handle: Handle,
        overlapped: *mut Overlapped,
        count: *mut u32,
        wait: i32,
    ) -> i32;
    fn CancelIoEx(handle: Handle, overlapped: *mut Overlapped) -> i32;
    fn GetNamedPipeServerProcessId(handle: Handle, pid: *mut u32) -> i32;
    fn WaitNamedPipeW(name: *const u16, timeout: u32) -> i32;
    fn GetCurrentProcess() -> Handle;
    fn LocalFree(memory: *mut c_void) -> *mut c_void;
    fn OpenProcess(access: u32, inherit: i32, pid: u32) -> Handle;
    fn QueryFullProcessImageNameW(
        handle: Handle,
        flags: u32,
        path: *mut u16,
        length: *mut u32,
    ) -> i32;
    fn GetProcessTimes(
        handle: Handle,
        creation: *mut FileTime,
        exit: *mut FileTime,
        kernel: *mut FileTime,
        user: *mut FileTime,
    ) -> i32;
    fn GetExitCodeProcess(handle: Handle, exit: *mut u32) -> i32;
}
#[link(name = "advapi32")]
extern "system" {
    fn OpenProcessToken(process: Handle, access: u32, token: *mut Handle) -> i32;
    fn GetTokenInformation(
        token: Handle,
        class: u32,
        buffer: *mut c_void,
        length: u32,
        required: *mut u32,
    ) -> i32;
    fn ConvertSidToStringSidW(sid: *mut c_void, string: *mut *mut u16) -> i32;
}
#[link(name = "iphlpapi")]
extern "system" {
    fn GetExtendedTcpTable(
        table: *mut c_void,
        length: *mut u32,
        sort: i32,
        family: u32,
        class: u32,
        reserved: u32,
    ) -> u32;
}

struct Owned(Handle);
impl Drop for Owned {
    fn drop(&mut self) {
        if !self.0.is_null() && self.0 as isize != -1 {
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
}
unsafe impl Send for Owned {}

pub(super) struct Pipe {
    handle: Owned,
}
impl Pipe {
    pub(super) fn open(path: &str) -> io::Result<Self> {
        let wide: Vec<u16> = path.encode_utf16().chain(Some(0)).collect();
        unsafe {
            let mut handle = CreateFileW(
                wide.as_ptr(),
                0xC0000000,
                0,
                ptr::null(),
                3,
                0x40000000 | 0x00110000,
                ptr::null_mut(),
            );
            if handle as isize == -1 && io::Error::last_os_error().raw_os_error() == Some(231) {
                WaitNamedPipeW(wide.as_ptr(), 2000);
                handle = CreateFileW(
                    wide.as_ptr(),
                    0xC0000000,
                    0,
                    ptr::null(),
                    3,
                    0x40000000 | 0x00110000,
                    ptr::null_mut(),
                );
            }
            if handle as isize == -1 {
                Err(io::Error::last_os_error())
            } else {
                Ok(Self {
                    handle: Owned(handle),
                })
            }
        }
    }
    pub(super) fn server_pid(&self) -> Option<u32> {
        let mut pid = 0;
        (unsafe { GetNamedPipeServerProcessId(self.handle.0, &mut pid) } != 0 && pid != 0)
            .then_some(pid)
    }
    fn operation(
        &self,
        buffer: *mut c_void,
        length: usize,
        write: bool,
        deadline: Instant,
    ) -> io::Result<usize> {
        let timeout = deadline
            .checked_duration_since(Instant::now())
            .ok_or(io::ErrorKind::TimedOut)?;
        let event = Owned(unsafe { CreateEventW(ptr::null(), 1, 0, ptr::null()) });
        if event.0.is_null() {
            return Err(io::Error::last_os_error());
        }
        let mut overlapped = Overlapped {
            internal: 0,
            internal_high: 0,
            offset: 0,
            offset_high: 0,
            event: event.0,
        };
        let mut count = 0;
        let result = unsafe {
            if write {
                WriteFile(
                    self.handle.0,
                    buffer,
                    length.min(u32::MAX as usize) as u32,
                    &mut count,
                    &mut overlapped,
                )
            } else {
                ReadFile(
                    self.handle.0,
                    buffer,
                    length.min(u32::MAX as usize) as u32,
                    &mut count,
                    &mut overlapped,
                )
            }
        };
        if result == 0 {
            let error = io::Error::last_os_error();
            match error.raw_os_error() {
                Some(109) if !write => return Ok(0),
                Some(234) if !write => return Ok(count as usize),
                Some(997) => {}
                _ => return Err(error),
            }
            let status = unsafe {
                WaitForSingleObject(
                    event.0,
                    timeout.as_millis().clamp(1, u32::MAX as u128 - 1) as u32,
                )
            };
            if status != 0 {
                unsafe {
                    CancelIoEx(self.handle.0, &mut overlapped);
                    GetOverlappedResult(self.handle.0, &mut overlapped, &mut count, 1);
                }
                return Err(if status == 258 {
                    io::ErrorKind::TimedOut.into()
                } else {
                    io::Error::last_os_error()
                });
            }
        }
        if unsafe { GetOverlappedResult(self.handle.0, &mut overlapped, &mut count, 1) } == 0 {
            let error = io::Error::last_os_error();
            if !write && error.raw_os_error() == Some(109) {
                return Ok(0);
            }
            if write || error.raw_os_error() != Some(234) {
                return Err(error);
            }
        }
        Ok(count as usize)
    }
    pub(super) fn read(&mut self, buffer: &mut [u8], deadline: Instant) -> io::Result<usize> {
        self.operation(buffer.as_mut_ptr().cast(), buffer.len(), false, deadline)
    }
    pub(super) fn write(&mut self, buffer: &[u8], deadline: Instant) -> io::Result<usize> {
        self.operation(buffer.as_ptr() as *mut c_void, buffer.len(), true, deadline)
    }
}

pub(super) fn default_pipes() -> Result<Vec<String>, String> {
    let fail = || "无法获取当前 Windows 用户的身份。".to_string();
    let mut handle = ptr::null_mut();
    if unsafe { OpenProcessToken(GetCurrentProcess(), 8, &mut handle) } == 0 {
        return Err(fail());
    }
    let token = Owned(handle);
    let mut length = 0;
    unsafe {
        GetTokenInformation(token.0, 1, ptr::null_mut(), 0, &mut length);
    }
    if length == 0 {
        return Err(fail());
    }
    let mut buffer = vec![0u8; length as usize];
    if unsafe { GetTokenInformation(token.0, 1, buffer.as_mut_ptr().cast(), length, &mut length) }
        == 0
    {
        return Err(fail());
    }
    let sid = unsafe { ptr::read_unaligned(buffer.as_ptr().cast::<Handle>()) };
    let mut string = ptr::null_mut();
    if unsafe { ConvertSidToStringSidW(sid, &mut string) } == 0 {
        return Err(fail());
    }
    let value = unsafe {
        let mut length = 0;
        while *string.add(length) != 0 {
            length += 1;
        }
        String::from_utf16_lossy(std::slice::from_raw_parts(string, length))
    };
    unsafe {
        LocalFree(string.cast());
    }
    let owner = format!("{:x}", Sha256::digest(value.as_bytes()));
    Ok(
        ["verge-mihomo-production-", "verge-mihomo-sidecar-release-"]
            .into_iter()
            .map(|prefix| format!(r"\\.\pipe\{prefix}{owner}"))
            .collect(),
    )
}

pub(super) fn tcp_owners(needed: &HashSet<SocketKey>) -> HashMap<SocketKey, Option<u32>> {
    let mut result = HashMap::new();
    if needed.is_empty() {
        return result;
    }
    let mut add = |key: SocketKey, pid: u32| {
        if pid == 0 || !needed.contains(&key) {
            return;
        }
        let entry = result.entry(key).or_insert(Some(pid));
        if *entry != Some(pid) {
            *entry = None;
        }
    };
    for family in [2, 23] {
        let mut length = 0;
        let code = unsafe { GetExtendedTcpTable(ptr::null_mut(), &mut length, 0, family, 5, 0) };
        if code != 122 || length < 4 {
            continue;
        }
        let mut buffer = vec![0u8; length as usize];
        if unsafe { GetExtendedTcpTable(buffer.as_mut_ptr().cast(), &mut length, 0, family, 5, 0) }
            != 0
        {
            continue;
        }
        let count = unsafe { ptr::read_unaligned(buffer.as_ptr().cast::<u32>()) } as usize;
        let size = if family == 2 {
            std::mem::size_of::<Tcp4>()
        } else {
            std::mem::size_of::<Tcp6>()
        };
        if count
            .checked_mul(size)
            .and_then(|size| size.checked_add(4))
            .is_none_or(|size| size > buffer.len())
        {
            continue;
        }
        for index in 0..count {
            let pointer = unsafe { buffer.as_ptr().add(4 + index * size) };
            if family == 2 {
                let row = unsafe { ptr::read_unaligned(pointer.cast::<Tcp4>()) };
                if row.state != 5 {
                    continue;
                }
                add(
                    SocketKey {
                        local: SocketAddr::new(
                            IpAddr::V4(Ipv4Addr::from(row.local.to_ne_bytes())),
                            u16::from_be(row.local_port as u16),
                        ),
                        peer: SocketAddr::new(
                            IpAddr::V4(Ipv4Addr::from(row.remote.to_ne_bytes())),
                            u16::from_be(row.remote_port as u16),
                        ),
                    },
                    row.pid,
                );
            } else {
                let row = unsafe { ptr::read_unaligned(pointer.cast::<Tcp6>()) };
                if row.state != 5 {
                    continue;
                }
                let normalize = |bytes: [u8; 16]| {
                    let v6 = Ipv6Addr::from(bytes);
                    v6.to_ipv4_mapped()
                        .map(IpAddr::V4)
                        .unwrap_or(IpAddr::V6(v6))
                };
                add(
                    SocketKey {
                        local: SocketAddr::new(
                            normalize(row.local),
                            u16::from_be(row.local_port as u16),
                        ),
                        peer: SocketAddr::new(
                            normalize(row.remote),
                            u16::from_be(row.remote_port as u16),
                        ),
                    },
                    row.pid,
                );
            }
        }
    }
    result
}
pub(super) fn process_details(pid: u32) -> Option<(String, f64)> {
    let process = Owned(unsafe { OpenProcess(0x1000, 0, pid) });
    if process.0.is_null() {
        return None;
    }
    let mut path = vec![0u16; 32768];
    let mut length = path.len() as u32;
    if unsafe { QueryFullProcessImageNameW(process.0, 0, path.as_mut_ptr(), &mut length) } == 0 {
        return None;
    }
    let mut creation = FileTime::default();
    let mut exit = FileTime::default();
    let mut kernel = FileTime::default();
    let mut user = FileTime::default();
    if unsafe { GetProcessTimes(process.0, &mut creation, &mut exit, &mut kernel, &mut user) } == 0
    {
        return None;
    }
    let mut code = 0;
    if unsafe { GetExitCodeProcess(process.0, &mut code) } == 0 || code != 259 {
        return None;
    }
    let ticks = ((creation.high as u64) << 32) | creation.low as u64;
    let name = app_name(&String::from_utf16_lossy(&path[..length as usize]))?;
    Some((name, ticks as f64 / 10_000_000.0 - 11_644_473_600.0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        net::{TcpListener, TcpStream},
        sync::{
            atomic::{AtomicUsize, Ordering},
            mpsc,
        },
        thread,
        time::Duration,
    };
    #[link(name = "kernel32")]
    extern "system" {
        fn CreateNamedPipeW(
            name: *const u16,
            mode: u32,
            pipe_mode: u32,
            max_instances: u32,
            out_size: u32,
            in_size: u32,
            timeout: u32,
            security: *const c_void,
        ) -> Handle;
        fn ConnectNamedPipe(handle: Handle, overlapped: *mut c_void) -> i32;
        fn DisconnectNamedPipe(handle: Handle) -> i32;
    }
    static SEQUENCE: AtomicUsize = AtomicUsize::new(0);
    fn server(
        response: Option<Vec<u8>>,
        message_mode: bool,
    ) -> (String, mpsc::Sender<()>, thread::JoinHandle<()>) {
        let name = format!(
            r"\\.\pipe\clash-monitor-core-test-{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::SeqCst)
        );
        let wide: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
        let raw = unsafe {
            CreateNamedPipeW(
                wide.as_ptr(),
                3,
                if message_mode { 6 } else { 0 },
                1,
                8192,
                8192,
                0,
                ptr::null(),
            )
        };
        assert_ne!(raw as isize, -1, "{}", io::Error::last_os_error());
        let raw = raw as usize;
        let (tx, rx) = mpsc::channel();
        let join = thread::spawn(move || {
            let handle = Owned(raw as Handle);
            if unsafe { ConnectNamedPipe(handle.0, ptr::null_mut()) } == 0 {
                assert_eq!(io::Error::last_os_error().raw_os_error(), Some(535));
            }
            let mut request = [0u8; 4096];
            let mut count = 0;
            assert_ne!(
                unsafe {
                    ReadFile(
                        handle.0,
                        request.as_mut_ptr().cast(),
                        request.len() as u32,
                        &mut count,
                        ptr::null_mut(),
                    )
                },
                0
            );
            assert!(count > 0);
            if let Some(body) = response {
                let mut written = 0;
                assert_ne!(
                    unsafe {
                        WriteFile(
                            handle.0,
                            body.as_ptr().cast(),
                            body.len() as u32,
                            &mut written,
                            ptr::null_mut(),
                        )
                    },
                    0
                );
                assert_eq!(written, body.len() as u32);
            }
            rx.recv_timeout(Duration::from_secs(5)).unwrap();
            unsafe {
                DisconnectNamedPipe(handle.0);
            }
        });
        (name, tx, join)
    }

    #[test]
    fn native_named_pipe_http_length_and_chunked_finish_before_server_closes() {
        for chunked in [false, true] {
            let body = "{\"目标\":\"例子.测试\"}".as_bytes();
            let mut response = if chunked {
                format!(
                    "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n{:x}\r\n",
                    body.len()
                )
                .into_bytes()
            } else {
                format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", body.len()).into_bytes()
            };
            response.extend_from_slice(body);
            if chunked {
                response.extend_from_slice(b"\r\n0\r\n\r\n");
            }
            let (name, stop, join) = server(Some(response), chunked);
            let mut endpoint = crate::source::PipeEndpoint {
                path: name,
                pid: None,
            };
            let now = Instant::now();
            let value = crate::source::Transport::get(&mut endpoint, "/connections").unwrap();
            assert!(now.elapsed() < Duration::from_secs(2));
            assert_eq!(value["目标"], "例子.测试");
            assert_eq!(endpoint.pid, Some(std::process::id()));
            stop.send(()).unwrap();
            join.join().unwrap();
        }
    }
    #[test]
    fn native_overlapped_timeout_cancels_and_drains_without_leaked_buffer() {
        let (name, stop, join) = server(None, false);
        let mut pipe = Pipe::open(&name).unwrap();
        pipe.write(
            b"GET /connections HTTP/1.1\r\n\r\n",
            Instant::now() + Duration::from_secs(1),
        )
        .unwrap();
        let mut buffer = [0u8; 8];
        let now = Instant::now();
        let error = pipe
            .read(&mut buffer, now + Duration::from_millis(120))
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert!(now.elapsed() < Duration::from_secs(1));
        drop(pipe);
        stop.send(()).unwrap();
        join.join().unwrap();
    }
    #[test]
    fn native_tcp_four_tuple_finds_owner_and_process_creation() {
        for address in ["127.0.0.1:0", "[::1]:0"] {
            let listener = TcpListener::bind(address).unwrap();
            let client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
            let (_accepted, _) = listener.accept().unwrap();
            let key = SocketKey {
                local: client.local_addr().unwrap(),
                peer: client.peer_addr().unwrap(),
            };
            let owners = tcp_owners(&HashSet::from([key]));
            assert_eq!(owners.get(&key), Some(&Some(std::process::id())), "{key:?}");
        }
        let (name, created) = process_details(std::process::id()).unwrap();
        assert!(name.ends_with(".exe"));
        assert!(created <= crate::time::unix_now());
        assert!(process_details(u32::MAX).is_none());
    }
    #[test]
    fn native_sid_paths_match_existing_verge_pipe_identity() {
        let paths = default_pipes().unwrap();
        assert_eq!(paths.len(), 2);
        assert!(paths[0].starts_with(r"\\.\pipe\verge-mihomo-production-"));
        let hash = paths[0].rsplit('-').next().unwrap();
        assert_eq!(hash.len(), 64);
        assert!(hash.bytes().all(|b| b.is_ascii_hexdigit()));
        assert!(paths[1].ends_with(hash));
    }
}
