//! 开机自启：HKCU Run 键。移植自 C# AutoStartManager.cs。

use crate::log::log;
use windows_sys::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyW, RegDeleteValueW, RegSetValueExW, HKEY, HKEY_CURRENT_USER, REG_SZ,
};

const RUN_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
const VALUE_NAME: &str = "Curosu";

/// 应用自启设置。返回是否成功。
pub fn apply(enabled: bool) -> bool {
    unsafe {
        let key_wide: Vec<u16> = RUN_KEY.encode_utf16().chain(std::iter::once(0)).collect();
        let value_wide: Vec<u16> = VALUE_NAME
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        let mut hkey: HKEY = std::ptr::null_mut();
        let rc = RegCreateKeyW(HKEY_CURRENT_USER, key_wide.as_ptr(), &mut hkey);
        if rc != 0 || hkey.is_null() {
            log("autostart: RegCreateKeyW failed");
            return false;
        }
        let result = if enabled {
            let path = startup_path();
            if path.is_empty() {
                RegCloseKey(hkey);
                return false;
            }
            // Run 键的可执行文件路径含空格（Program Files）时必须加引号，
            // 否则 Windows 会把它解析为 C:\\Program.exe 及其余参数。
            let command_wide = startup_command(&path)
                .encode_utf16()
                .chain(std::iter::once(0))
                .collect::<Vec<u16>>();
            RegSetValueExW(
                hkey,
                value_wide.as_ptr(),
                0,
                REG_SZ,
                command_wide.as_ptr() as *const u8,
                (command_wide.len() * 2) as u32,
            )
        } else {
            RegDeleteValueW(hkey, value_wide.as_ptr())
        };
        RegCloseKey(hkey);
        result == 0
    }
}

fn startup_path() -> String {
    let installed = r"C:\Program Files\Curosu\curosu.exe";
    if std::path::Path::new(installed).exists() {
        return installed.to_string();
    }
    std::env::current_exe()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default()
}

fn startup_command(path: &str) -> String {
    format!("\"{path}\"")
}

#[cfg(test)]
mod tests {
    use super::startup_command;

    #[test]
    fn startup_command_quotes_paths_with_spaces() {
        assert_eq!(
            startup_command(r"C:\Program Files\Curosu\curosu.exe"),
            r#""C:\Program Files\Curosu\curosu.exe""#
        );
    }
}
