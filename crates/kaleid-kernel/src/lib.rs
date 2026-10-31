//! # kaleid-kernel
//!
//! Kaleid 的插件内核。设计取自 DeepSeek Harness 所用的 Cordis 语义，并落到 Rust：
//!
//! - [`Context::provide`] —— 按类型注册服务，`priority` 更大的遮蔽较小的；
//! - [`Context::services`] —— 拿到同一条服务链，用于"包裹/修改"而非"替换"；
//! - [`Effect`] —— 一切注册都是可撤销副作用，插件停用时逆序回滚；
//! - [`Context::on`] / [`Context::emit`] —— 按载荷类型寻址的事件总线；
//! - [`patch`] —— 基线 + 补丁的声明式组合，取代"复制一份完整清单"。
//!
//! 内核**不认识任何 Minecraft 概念**。版本扫描、下载源、启动参数全部在
//! `kaleid-core-plugins` 里以服务形式提供，因此都能被第三方插件遮蔽或包裹。

pub mod context;
pub mod effect;
pub mod event;
pub mod patch;
pub mod plugin;
pub mod schema;

pub use context::{merge_json, Context, Kernel, KernelError, ServiceHandle, ServiceKey};
pub use effect::{Effect, EffectScope};
pub use patch::{apply_patches, parse_patch, PatchError, PatchOp, UpdateOp};
pub use plugin::{
    Plugin, PluginFactory, PluginId, PluginResult, PluginSpec, PluginState, Predicate,
};
pub use schema::SchemaError;
