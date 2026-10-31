//! 数据根路径解析。

use std::path::PathBuf;

/// 应用数据根：`%LOCALAPPDATA%\Kaleid\`。
pub fn data_root() -> PathBuf {
    if let Ok(custom) = std::env::var("KALEID_DATA_DIR") {
        if !custom.is_empty() {
            return PathBuf::from(custom);
        }
    }
    if let Ok(local) = std::env::var("LOCALAPPDATA") {
        if !local.is_empty() {
            return PathBuf::from(local).join("Kaleid");
        }
    }
    PathBuf::from(".kaleid-data")
}

pub fn settings_file(root: &std::path::Path) -> PathBuf {
    root.join("settings.json")
}

pub fn roots_file(root: &std::path::Path) -> PathBuf {
    root.join("roots.json")
}

pub fn accounts_file(root: &std::path::Path) -> PathBuf {
    root.join("accounts.json")
}

pub fn version_settings_file(root: &std::path::Path, root_id: &str, version_id: &str) -> PathBuf {
    root.join("version-settings")
        .join(root_id)
        .join(format!("{version_id}.json"))
}
