//! 公共错误类型。

use serde::Serialize;

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("路径不可用：{0}")]
    Path(String),
    #[error("读写失败：{0}")]
    Io(String),
    #[error("数据格式不合法：{0}")]
    Format(String),
    #[error("未找到：{0}")]
    NotFound(String),
    #[error("非法输入：{0}")]
    Invalid(String),
    /// 需要用户先做一个决定（例如「覆盖同名文件」）。文案自己写完整句子，
    /// 所以这里不加前缀，直接原样展示。
    #[error("{0}")]
    Conflict(String),
    #[error("插件未就绪：{0}")]
    Unavailable(String),
    #[error("网络请求失败：{0}")]
    Network(String),
}

impl ApiError {
    pub fn io(e: impl std::fmt::Display) -> Self {
        ApiError::Io(e.to_string())
    }
    pub fn invalid(msg: impl Into<String>) -> Self {
        ApiError::Invalid(msg.into())
    }
    pub fn not_found(msg: impl Into<String>) -> Self {
        ApiError::NotFound(msg.into())
    }

    pub fn conflict(msg: impl Into<String>) -> Self {
        ApiError::Conflict(msg.into())
    }
}

impl Serialize for ApiError {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_string())
    }
}

impl From<std::io::Error> for ApiError {
    fn from(e: std::io::Error) -> Self {
        ApiError::Io(e.to_string())
    }
}

impl From<serde_json::Error> for ApiError {
    fn from(e: serde_json::Error) -> Self {
        ApiError::Format(e.to_string())
    }
}

pub type ApiResult<T> = Result<T, ApiError>;
