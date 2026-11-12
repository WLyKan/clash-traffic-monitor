//! Per-data-directory focus handoff. The SQLite writer lock remains authoritative;
//! this endpoint can only show the existing window, never read or mutate data.
use rand::RngCore;
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::{Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread::{self, JoinHandle},
    time::Duration,
};
#[derive(Serialize, Deserialize)]
struct Endpoint {
    port: u16,
    token: String,
}
pub struct InstanceServer {
    path: PathBuf,
    token: String,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}
impl InstanceServer {
    /// Call only after acquiring WriterLock. Each isolated directory gets its
    /// own endpoint, so tests and production never steal each other's window.
    pub fn start(directory: &Path, on_focus: impl Fn() + Send + 'static) -> Result<Self, String> {
        let listener =
            TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).map_err(|e| e.to_string())?;
        listener.set_nonblocking(true).map_err(|e| e.to_string())?;
        let mut random = [0u8; 32];
        rand::rng().fill_bytes(&mut random);
        let token = random
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        let path = directory.join("instance.json");
        let endpoint = Endpoint {
            port: listener.local_addr().map_err(|e| e.to_string())?.port(),
            token: token.clone(),
        };
        fs::write(
            &path,
            serde_json::to_vec(&endpoint).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = stop.clone();
        let secret = token.clone();
        let worker = thread::Builder::new()
            .name("instance-focus".into())
            .spawn(move || {
                while !stopping.load(Ordering::Acquire) {
                    match listener.accept() {
                        Ok((mut stream, _)) => {
                            let _ = stream.set_read_timeout(Some(Duration::from_millis(200)));
                            let _ = stream.set_write_timeout(Some(Duration::from_millis(200)));
                            let mut request = [0u8; 64];
                            if stream.read_exact(&mut request).is_ok()
                                && request.as_slice() == secret.as_bytes()
                            {
                                on_focus();
                                let _ = stream.write_all(b"ok");
                            }
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(40))
                        }
                        Err(_) => break,
                    }
                }
            })
            .map_err(|e| e.to_string())?;
        Ok(Self {
            path,
            token,
            stop,
            worker: Some(worker),
        })
    }
}
impl Drop for InstanceServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(handle) = self.worker.take() {
            let _ = handle.join();
        }
        if fs::read(&self.path)
            .ok()
            .and_then(|b| serde_json::from_slice::<Endpoint>(&b).ok())
            .is_some_and(|e| e.token == self.token)
        {
            let _ = fs::remove_file(&self.path);
        }
    }
}
pub fn focus_existing(directory: &Path) -> bool {
    let result = || -> Option<()> {
        let bytes = fs::read(directory.join("instance.json")).ok()?;
        if bytes.len() > 1024 {
            return None;
        }
        let endpoint: Endpoint = serde_json::from_slice(&bytes).ok()?;
        if endpoint.token.len() != 64 {
            return None;
        }
        let address = SocketAddr::from((std::net::Ipv4Addr::LOCALHOST, endpoint.port));
        let mut stream = TcpStream::connect_timeout(&address, Duration::from_millis(200)).ok()?;
        stream
            .set_read_timeout(Some(Duration::from_millis(300)))
            .ok()?;
        stream
            .set_write_timeout(Some(Duration::from_millis(300)))
            .ok()?;
        stream.write_all(endpoint.token.as_bytes()).ok()?;
        let mut reply = [0; 2];
        stream.read_exact(&mut reply).ok()?;
        if &reply == b"ok" {
            Some(())
        } else {
            None
        }
    };
    result().is_some()
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn isolate_handoff_reject_invalid_token_and_cleanup() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let shared = count.clone();
        let server = InstanceServer::start(a.path(), move || {
            shared.fetch_add(1, Ordering::SeqCst);
        })
        .unwrap();
        assert!(!focus_existing(b.path()));
        assert!(focus_existing(a.path()));
        assert_eq!(count.load(Ordering::SeqCst), 1);
        let original = fs::read(a.path().join("instance.json")).unwrap();
        let mut endpoint: Endpoint = serde_json::from_slice(&original).unwrap();
        endpoint.token = "0".repeat(64);
        fs::write(
            a.path().join("instance.json"),
            serde_json::to_vec(&endpoint).unwrap(),
        )
        .unwrap();
        assert!(!focus_existing(a.path()));
        assert_eq!(count.load(Ordering::SeqCst), 1);
        fs::write(a.path().join("instance.json"), original).unwrap();
        drop(server);
        assert!(!a.path().join("instance.json").exists());
        assert!(!focus_existing(a.path()));
    }
}
