//! 插件契约、生命周期与组合规格（`PluginSpec`）。

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;

use crate::context::Context;

/// 插件标识，进程内唯一。
pub type PluginId = String;

/// 插件生命周期状态。与插件管理页展示的状态一一对应。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginState {
    /// 已登记，尚未启动。
    Registered,
    /// 依赖已满足，正在启动。
    Starting,
    /// 已就绪，服务可用。
    Ready,
    /// 被显式停用（服务与席位已回收）。
    Disabled,
    /// 启动失败。
    Failed,
}

impl PluginState {
    pub fn is_active(self) -> bool {
        matches!(self, PluginState::Ready | PluginState::Starting)
    }
}

/// 插件启动失败的错误类型，交由内核包装上报。
pub type PluginResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

/// 一切可插拔单元的统一契约。
///
/// `apply` 是**同步**的：内核生命周期不绑定任何异步运行时。
/// 需要异步能力的服务自己持有运行时（例如下载队列），内核只负责注册与回收。
pub trait Plugin: Send + Sync {
    /// 稳定 id，同时作为补丁、依赖声明、席位归属的键。
    fn id(&self) -> &str;

    /// 依赖的**其他插件 id**，内核据此拓扑排序。
    fn inject(&self) -> &'static [&'static str] {
        &[]
    }

    /// 配置的 JSON-Schema（封闭子集，见 [`crate::schema`]）。
    fn schema(&self) -> Option<Value> {
        None
    }

    /// 默认配置，用于补齐缺失字段。
    fn default_config(&self) -> Value {
        Value::Object(Default::default())
    }

    /// 启动。期间通过 `ctx` 注册的一切都会在该插件停用时自动回滚。
    fn apply(&self, ctx: &Context, cfg: Value) -> PluginResult;
}

/// 一条插件组合规格，来自 `presets/*.patch.yml` 或内置组合。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginSpec {
    /// 规格 id（默认等于 `name`）。
    #[serde(default)]
    pub id: String,
    /// 工厂名，对应 `Kernel::register_factory` 注册的名字。
    pub name: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub config: Value,
    /// 只有全部条件成立时才插入。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub when: Vec<Predicate>,
    /// 必需插件：停用它不是「少一块功能」，而是启动器直接残废。
    ///
    /// 放在规格里而不是只放界面里，是因为界面不是唯一的调用方——命令层、
    /// 无头 CLI、补丁文件都能改插件开关；只在前端置灰等于没拦。
    #[serde(default, skip_serializing_if = "is_false")]
    pub required: bool,
}

fn default_true() -> bool {
    true
}

fn is_false(value: &bool) -> bool {
    !*value
}

impl PluginSpec {
    pub fn new(name: &str) -> Self {
        PluginSpec {
            id: name.to_string(),
            name: name.to_string(),
            enabled: true,
            config: Value::Object(Default::default()),
            when: Vec::new(),
            required: false,
        }
    }

    /// 标成必需插件：`Kernel::disable` 会拒绝停用它。
    pub fn required(mut self) -> Self {
        self.required = true;
        self
    }

    pub fn with_config(mut self, config: Value) -> Self {
        self.config = config;
        self
    }

    pub fn with_id(mut self, id: &str) -> Self {
        self.id = id.to_string();
        self
    }

    pub fn disabled(mut self) -> Self {
        self.enabled = false;
        self
    }
}

/// 受限条件谓词。**刻意不引入 `!!js`**：补丁必须是纯声明、可静态审计的。
///
/// YAML 里写作单键映射（`- platform: windows`）。YAML 没有 serde 的外部标记枚举语法，
/// 所以这里用一个 `untagged` 的中间类型手写 `Deserialize`，让 YAML 与 JSON 两种
/// 载体都能接受同一种自然写法。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Predicate {
    /// 当前平台为 `windows` / `linux` / `macos`。
    Platform { platform: String },
    /// 环境变量等于给定值（未设置视为空串）。
    EnvEq { env: String, eq: String },
    /// 环境变量已设置且非空。
    EnvSet { env: String },
}

#[derive(Deserialize)]
#[serde(untagged)]
enum RawPredicate {
    Platform { platform: String },
    EnvEq { env_eq: EnvEqInner },
    EnvSet { env_set: String },
}

#[derive(Deserialize)]
struct EnvEqInner {
    env: String,
    eq: String,
}

impl<'de> Deserialize<'de> for Predicate {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Ok(match RawPredicate::deserialize(d)? {
            RawPredicate::Platform { platform } => Predicate::Platform { platform },
            RawPredicate::EnvEq { env_eq } => Predicate::EnvEq {
                env: env_eq.env,
                eq: env_eq.eq,
            },
            RawPredicate::EnvSet { env_set } => Predicate::EnvSet { env: env_set },
        })
    }
}

impl Serialize for Predicate {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        match self {
            Predicate::Platform { platform } => {
                let mut m = s.serialize_map(Some(1))?;
                m.serialize_entry("platform", platform)?;
                m.end()
            }
            Predicate::EnvEq { env, eq } => {
                #[derive(Serialize)]
                struct Inner<'a> {
                    env: &'a str,
                    eq: &'a str,
                }
                let mut m = s.serialize_map(Some(1))?;
                m.serialize_entry("env_eq", &Inner { env, eq })?;
                m.end()
            }
            Predicate::EnvSet { env } => {
                let mut m = s.serialize_map(Some(1))?;
                m.serialize_entry("env_set", env)?;
                m.end()
            }
        }
    }
}

impl Predicate {
    pub fn evaluate(&self) -> bool {
        match self {
            Predicate::Platform { platform } => std::env::consts::OS == platform,
            Predicate::EnvEq { env, eq } => std::env::var(env).unwrap_or_default() == *eq,
            Predicate::EnvSet { env } => std::env::var(env).map(|v| !v.is_empty()).unwrap_or(false),
        }
    }
}

/// 工厂：按名字造插件实例。
pub type PluginFactory = Arc<dyn Fn() -> Arc<dyn Plugin> + Send + Sync>;
