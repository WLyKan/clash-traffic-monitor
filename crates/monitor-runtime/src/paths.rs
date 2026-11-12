use fs2::FileExt;
use std::{
    fs::{self, File, OpenOptions},
    path::{Path, PathBuf},
};

pub fn default_data_dir() -> Result<PathBuf, String> {
    let profile = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .ok_or("无法确定用户目录，请指定 --data-dir")?;
    let home = PathBuf::from(profile);
    if !home.is_absolute() {
        return Err("用户目录必须是绝对路径".into());
    }
    Ok(home.join("Documents").join("ClashTrafficMonitor"))
}
pub fn resolve_data_dir(directory: Option<&Path>) -> Result<PathBuf, String> {
    let path = match directory {
        Some(p) => p.to_path_buf(),
        None => default_data_dir()?,
    };
    let absolute = if path.is_absolute() {
        path
    } else {
        std::env::current_dir()
            .map_err(|e| e.to_string())?
            .join(path)
    };
    // Resolve an existing parent too: the final directory may not exist yet,
    // while Documents itself can be a junction. Diagnostics must not create it.
    let mut ancestor = absolute.as_path();
    while !ancestor.exists() {
        ancestor = ancestor.parent().ok_or("无法解析数据目录的物理位置")?;
    }
    let physical = dunce::canonicalize(ancestor).map_err(|e| e.to_string())?;
    let remainder = absolute.strip_prefix(ancestor).map_err(|e| e.to_string())?;
    Ok(physical.join(remainder))
}
/// Windows fs2 locks include byte zero, interoperating with the legacy MSVCRT
/// first-byte lock. Keep this object alive until collection and shutdown finish.
pub struct WriterLock {
    _file: File,
}
impl WriterLock {
    pub fn acquire(directory: &Path) -> Result<Self, String> {
        fs::create_dir_all(directory).map_err(|e| format!("无法创建数据目录：{e}"))?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(directory.join("collector.lock"))
            .map_err(|e| e.to_string())?;
        FileExt::try_lock_exclusive(&file)
            .map_err(|_| "该数据目录已有采集实例，请从托盘打开窗口".to_string())?;
        Ok(Self { _file: file })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn isolated_directories_and_same_physical_path() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        let lock = WriterLock::acquire(a.path()).unwrap();
        assert!(WriterLock::acquire(a.path()).is_err());
        assert!(WriterLock::acquire(b.path()).is_ok());
        drop(lock);
        assert!(WriterLock::acquire(a.path()).is_ok());
    }
    #[test]
    fn resolve_diagnostics_does_not_create_missing_directory() {
        let root = tempfile::tempdir().unwrap();
        let missing = root.path().join("missing");
        assert_eq!(resolve_data_dir(Some(&missing)).unwrap(), missing);
        assert!(!missing.exists());
    }
}
