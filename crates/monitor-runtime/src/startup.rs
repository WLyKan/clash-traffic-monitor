use std::path::Path;
#[cfg(windows)]
use winreg::{enums::*, RegKey};
const VALUE: &str = "ClashTrafficMonitor";
pub fn enabled() -> Result<bool, String> {
    #[cfg(windows)]
    {
        let key = match RegKey::predef(HKEY_CURRENT_USER)
            .open_subkey("Software\\Microsoft\\Windows\\CurrentVersion\\Run")
        {
            Ok(key) => key,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(e) => return Err(e.to_string()),
        };
        match key.get_value::<String, _>(VALUE) {
            Ok(_) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e.to_string()),
        }
    }
    #[cfg(not(windows))]
    {
        Ok(false)
    }
}
pub fn command(exe: &Path, directory: &Path) -> String {
    format!(
        "\"{}\" --start-hidden --data-dir \"{}\"",
        exe.display(),
        directory.display()
    )
}
pub fn set_enabled(requested: bool, exe: &Path, directory: &Path) -> Result<(), String> {
    #[cfg(windows)]
    {
        let (key, _) = RegKey::predef(HKEY_CURRENT_USER)
            .create_subkey("Software\\Microsoft\\Windows\\CurrentVersion\\Run")
            .map_err(|e| e.to_string())?;
        if requested {
            key.set_value(VALUE, &command(exe, directory))
                .map_err(|e| e.to_string())
        } else {
            match key.delete_value(VALUE) {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(e.to_string()),
            }
        }
    }
    #[cfg(not(windows))]
    {
        let _ = (requested, exe, directory);
        Err("当前平台尚未实现登录启动".into())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn explicit_directory_and_spaces_quoted() {
        let cmd = command(
            Path::new("C:/Program Files/Monitor.exe"),
            Path::new("C:/Users/name/Documents/ClashTrafficMonitor"),
        );
        assert!(cmd.starts_with("\"C:/Program Files/Monitor.exe\""));
        assert!(cmd.contains("--start-hidden --data-dir \""));
    }
}
