//! Cross-runtime writer exclusion against the actual released Python entrypoint.
//!
//! Formal Windows acceptance must set CLASH_LEGACY_PYTHON to the original
//! repository's .venv\Scripts\python.exe. No live database or collector is used.
#![cfg(windows)]

use monitor_runtime::paths::{resolve_data_dir, WriterLock};
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    os::windows::{fs::MetadataExt, process::CommandExt},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::mpsc::{self, Receiver},
    thread,
    time::{Duration, Instant},
};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const PYTHON_HELPER: &str = r#"
import sys
from pathlib import Path
sys.path.insert(0, sys.argv[1])
from clash_monitor.__main__ import AlreadyRunning, single_writer
directory = Path(sys.argv[2])
try:
    with single_writer(directory):
        print('ACQUIRED\t' + str(directory.resolve()), flush=True)
        if sys.argv[3] == 'hold':
            sys.stdin.readline()
except AlreadyRunning:
    print('BLOCKED', flush=True)
    sys.exit(23)
"#;

struct Legacy {
    python: PathBuf,
    repository: PathBuf,
}
impl Legacy {
    fn configured() -> Option<Self> {
        let Some(python) = std::env::var_os("CLASH_LEGACY_PYTHON") else {
            eprintln!("SKIP: set CLASH_LEGACY_PYTHON to the original .venv\\Scripts\\python.exe for cross-runtime lock acceptance");
            return None;
        };
        let python = PathBuf::from(python);
        assert!(python.is_file(), "legacy Python does not exist: {python:?}");
        // Keep the configured venv path: canonicalizing python.exe could discard
        // the repository association on a Python installation using symlinks.
        let repository = python
            .parent()
            .and_then(Path::parent)
            .and_then(Path::parent)
            .expect("CLASH_LEGACY_PYTHON must identify .venv\\Scripts\\python.exe")
            .to_path_buf();
        assert!(
            repository.join("clash_monitor/__main__.py").is_file(),
            "configured Python is not inside the original repository: {repository:?}"
        );
        Some(Self { python, repository })
    }

    fn spawn(&self, directory: &Path, hold: bool) -> PythonLock {
        let mut child = Command::new(&self.python)
            .args(["-I", "-u", "-X", "utf8", "-c", PYTHON_HELPER])
            .arg(&self.repository)
            .arg(directory)
            .arg(if hold { "hold" } else { "probe" })
            .env("PYTHONIOENCODING", "utf-8")
            .current_dir(&self.repository)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .expect("start isolated legacy lock helper");
        let stdout = child.stdout.take().unwrap();
        let (sender, lines) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                match line {
                    Ok(line) => {
                        if sender.send(line).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });
        PythonLock { child, lines }
    }

    fn assert_probe(&self, directory: &Path, expected_acquired: bool) {
        let mut process = self.spawn(directory, false);
        let line = process.ready();
        assert_eq!(line.starts_with("ACQUIRED\t"), expected_acquired, "{line}");
        let code = process.finish();
        assert_eq!(code, if expected_acquired { 0 } else { 23 });
    }
}

/// Own exactly the helper we spawned; a failure cannot leave an orphaned lock.
struct PythonLock {
    child: Child,
    lines: Receiver<String>,
}
impl PythonLock {
    fn ready(&self) -> String {
        self.lines
            .recv_timeout(Duration::from_secs(15))
            .expect("legacy helper failed to report lock result within 15 seconds")
    }
    fn finish(&mut self) -> i32 {
        if let Some(mut stdin) = self.child.stdin.take() {
            // Closing stdin also releases a helper if a short write is rejected.
            let _ = stdin.write_all(b"release\n");
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(status) = self.child.try_wait().expect("wait for legacy lock helper") {
                return status
                    .code()
                    .expect("legacy helper terminated unexpectedly");
            }
            assert!(Instant::now() < deadline, "legacy helper did not exit");
            thread::sleep(Duration::from_millis(10));
        }
    }
}
impl Drop for PythonLock {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
    }
}

fn isolated_root() -> tempfile::TempDir {
    // The acceptance command sets TEMP/TMP to the task's D: cache. tempfile
    // obtains precisely that location, never Documents or the live data root.
    let base = std::env::temp_dir();
    fs::create_dir_all(&base).unwrap();
    tempfile::Builder::new()
        .prefix("clash-rust-python-lock-")
        .tempdir_in(base)
        .unwrap()
}

#[test]
fn python_msvcrt_and_rust_fs2_are_bidirectionally_exclusive_and_release() {
    let Some(legacy) = Legacy::configured() else {
        return;
    };
    let root = isolated_root();
    let first = root.path().join("first");
    let second = root.path().join("second");

    let rust = WriterLock::acquire(&first).unwrap();
    legacy.assert_probe(&first, false);
    // A simultaneous writer in another custom data directory is legitimate.
    legacy.assert_probe(&second, true);
    let independent = WriterLock::acquire(&second).unwrap();
    drop(independent);
    drop(rust);

    let mut python = legacy.spawn(&first, true);
    assert!(python.ready().starts_with("ACQUIRED\t"));
    assert!(WriterLock::acquire(&first).is_err());
    assert!(WriterLock::acquire(&second).is_ok());
    assert_eq!(python.finish(), 0);

    let rust = WriterLock::acquire(&first).unwrap();
    legacy.assert_probe(&first, false);
    drop(rust);
    legacy.assert_probe(&first, true);
    eprintln!("PASS: actual Python msvcrt <-> Rust fs2 mutual exclusion, release, and independent directories at {:?}", root.path());
}

struct Junction {
    path: PathBuf,
    root: PathBuf,
}
impl Junction {
    fn create(root: &Path, target: &Path) -> Self {
        let root = dunce::canonicalize(root).unwrap();
        let target = dunce::canonicalize(target).unwrap();
        assert!(target.starts_with(&root));
        let path = root.join("same-physical-directory");
        assert!(path.starts_with(&root) && !path.exists());
        // Junctions require neither Developer Mode nor symlink privileges.
        // Both operands are owned, freshly generated task paths.
        let result = Command::new("cmd.exe")
            .args(["/D", "/C", "mklink", "/J"])
            .arg(&path)
            .arg(&target)
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "junction creation failed: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert_eq!(dunce::canonicalize(&path).unwrap(), target);
        Self { path, root }
    }
}
impl Drop for Junction {
    fn drop(&mut self) {
        // Remove only the verified reparse-point entry, never its target and
        // never a recursive path derived from an external input.
        if self.path.starts_with(&self.root)
            && fs::symlink_metadata(&self.path)
                .is_ok_and(|metadata| metadata.file_attributes() & 0x400 != 0)
        {
            fs::remove_dir(&self.path).expect("remove this test's junction entry");
        }
    }
}

#[test]
fn junction_and_canonical_directory_share_the_same_cross_runtime_lock() {
    let Some(legacy) = Legacy::configured() else {
        return;
    };
    let root = isolated_root();
    let physical = root.path().join("physical");
    fs::create_dir(&physical).unwrap();
    let junction = Junction::create(root.path(), &physical);
    assert_eq!(
        resolve_data_dir(Some(&junction.path)).unwrap(),
        resolve_data_dir(Some(&physical)).unwrap()
    );

    let rust = WriterLock::acquire(&physical).unwrap();
    legacy.assert_probe(&junction.path, false);
    drop(rust);

    let mut python = legacy.spawn(&junction.path, true);
    assert!(python.ready().starts_with("ACQUIRED\t"));
    assert!(WriterLock::acquire(&physical).is_err());
    assert_eq!(python.finish(), 0);

    let rust = WriterLock::acquire(&junction.path).unwrap();
    legacy.assert_probe(&physical, false);
    drop(rust);
    legacy.assert_probe(&physical, true);
    assert_eq!(
        fs::read_dir(&physical).unwrap().count(),
        1,
        "only the isolated collector.lock should exist"
    );
    eprintln!(
        "PASS: junction {:?} and physical {:?} share the same Python/Rust lock",
        junction.path, physical
    );
    drop(junction);
    assert!(!root.path().join("same-physical-directory").exists());
    assert!(physical.join("collector.lock").is_file());
}
