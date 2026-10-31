//! 极小的 JSON 持久化工具：原子写 + 缺失即默认。

use std::path::Path;

use serde::de::DeserializeOwned;
use serde::Serialize;

use kaleid_api::{ApiError, ApiResult};

/// 读 JSON。文件不存在返回 `Ok(None)`；内容损坏返回错误（**不**静默重置用户数据）。
pub fn read_json<T: DeserializeOwned>(path: &Path) -> ApiResult<Option<T>> {
    match std::fs::read_to_string(path) {
        Ok(text) => {
            if text.trim().is_empty() {
                return Ok(None);
            }
            serde_json::from_str::<T>(&text)
                .map(Some)
                .map_err(|e| ApiError::Format(format!("{}: {e}", path.display())))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(ApiError::Io(format!("{}: {e}", path.display()))),
    }
}

/// 原子写 JSON：先写同目录临时文件再重命名，避免半截文件。
pub fn write_json<T: Serialize>(path: &Path, value: &T) -> ApiResult<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let text = serde_json::to_string_pretty(value)
        .map_err(|e| ApiError::Format(format!("序列化失败：{e}")))?;
    let tmp = path.with_extension(format!(
        "{}.tmp",
        path.extension()
            .map(|e| e.to_string_lossy().to_string())
            .unwrap_or_else(|| "json".into())
    ));
    std::fs::write(&tmp, text.as_bytes())?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// 当前时间（秒级 RFC3339，够用且不引 `chrono`）。
pub fn now_iso() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("{secs}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn roundtrip_and_atomic_write() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a/b/c.json");
        assert!(read_json::<serde_json::Value>(&path).unwrap().is_none());
        write_json(&path, &json!({"x": 1})).unwrap();
        let back: serde_json::Value = read_json(&path).unwrap().unwrap();
        assert_eq!(back["x"], json!(1));
        // 临时文件不应残留
        let leftovers: Vec<_> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp"))
            .collect();
        assert!(leftovers.is_empty());
    }

    #[test]
    fn corrupt_file_is_reported_not_silently_reset() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.json");
        std::fs::write(&path, b"{ not json").unwrap();
        let err = read_json::<serde_json::Value>(&path).unwrap_err();
        assert!(matches!(err, ApiError::Format(_)));
    }
}
