//! 内核上下文与插件装载器。
//!
//! [`Context`] 是插件能看到的全部世界：注册服务、订阅事件、查询状态。
//! 插件 `apply` 期间产生的每一次注册都会被内核记在这个插件名下，
//! 插件停用时**逆序撤销**——这就是"停用插件后界面与服务一起消失"的机制基础。

use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::marker::PhantomData;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use serde_json::Value;

use crate::effect::Effect;
use crate::event::EventBus;
use crate::plugin::{Plugin, PluginFactory, PluginId, PluginResult, PluginSpec, PluginState};
use crate::schema;

/// 服务的人类可读名，仅用于诊断输出与日志。
pub type ServiceKey = &'static str;

/// 内核错误。
#[derive(Debug, thiserror::Error)]
pub enum KernelError {
    #[error("未注册的插件工厂 `{0}`")]
    UnknownFactory(String),
    #[error("插件依赖成环：{0}")]
    Cycle(String),
    #[error("插件 `{plugin}` 依赖的 `{dep}` 不存在")]
    MissingDependency { plugin: String, dep: String },
    #[error("插件 `{plugin}` 的配置非法：{reason}")]
    Config { plugin: String, reason: String },
    #[error("插件 `{plugin}` 启动失败：{reason}")]
    Apply { plugin: String, reason: String },
    #[error("插件 `{0}` 未装载")]
    NotLoaded(String),
    #[error("插件 `{0}` 是必需插件，不能停用")]
    Required(String),
}

struct ServiceEntry {
    seq: u64,
    owner: PluginId,
    service: ServiceKey,
    priority: i32,
    value: Arc<dyn Any + Send + Sync>,
}

pub(crate) struct Inner {
    services: RwLock<HashMap<TypeId, Vec<ServiceEntry>>>,
    effects: Mutex<HashMap<PluginId, Vec<Effect>>>,
    states: RwLock<HashMap<PluginId, PluginState>>,
    current: RwLock<Option<PluginId>>,
    events: Arc<EventBus>,
    seq: AtomicU64,
}

impl Inner {
    fn next_seq(&self) -> u64 {
        self.seq.fetch_add(1, Ordering::Relaxed) + 1
    }
}

/// 插件视角的上下文。`Clone` 是廉价的（共享同一个 `Arc<Inner>`）。
#[derive(Clone)]
pub struct Context {
    inner: Arc<Inner>,
}

impl Default for Context {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for Context {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Context")
            .field("plugins", &self.plugin_states().len())
            .finish()
    }
}

impl Context {
    pub fn new() -> Self {
        Context {
            inner: Arc::new(Inner {
                services: RwLock::new(HashMap::new()),
                effects: Mutex::new(HashMap::new()),
                states: RwLock::new(HashMap::new()),
                current: RwLock::new(None),
                events: Arc::new(EventBus::default()),
                seq: AtomicU64::new(0),
            }),
        }
    }

    fn owner_now(&self) -> PluginId {
        self.inner
            .current
            .read()
            .unwrap()
            .clone()
            .unwrap_or_default()
    }

    /// 以某个插件身份执行（内核装载与测试用）。
    pub fn with_owner<R>(&self, owner: &str, f: impl FnOnce(&Context) -> R) -> R {
        let previous = {
            let mut g = self.inner.current.write().unwrap();
            let prev = g.clone();
            *g = Some(owner.to_string());
            prev
        };
        let result = f(self);
        *self.inner.current.write().unwrap() = previous;
        result
    }

    /// 注册一个服务。`priority` 更大者胜出；消费者也可用 [`Context::services`] 拿到整条链。
    ///
    /// 返回值是这条注册的序号，便于测试断言。
    pub fn provide<T: Any + Send + Sync>(
        &self,
        service: ServiceKey,
        value: T,
        priority: i32,
    ) -> u64 {
        self.provide_owned(self.owner_now(), service, value, priority)
    }

    /// Register a kernel-lifetime service without inheriting or changing the ambient owner.
    /// Root registrations are reclaimed by Kernel shutdown/Drop, not plugin disposal.
    pub fn provide_root<T: Any + Send + Sync>(
        &self, service: ServiceKey, value: T, priority: i32,
    ) -> u64 {
        self.provide_owned(String::new(), service, value, priority)
    }

    fn provide_owned<T: Any + Send + Sync>(
        &self, owner: PluginId, service: ServiceKey, value: T, priority: i32,
    ) -> u64 {
        let seq = self.inner.next_seq();
        {
            let mut g = self.inner.services.write().unwrap();
            g.entry(TypeId::of::<T>()).or_default().push(ServiceEntry {
                seq,
                owner: owner.clone(),
                service,
                priority,
                value: Arc::new(value),
            });
        }
        let inner = Arc::clone(&self.inner);
        let tid = TypeId::of::<T>();
        let effect = Effect::new(move || {
            if let Ok(mut g) = inner.services.write() {
                if let Some(list) = g.get_mut(&tid) {
                    list.retain(|e| e.seq != seq);
                }
            }
        });
        self.inner
            .effects
            .lock()
            .unwrap()
            .entry(owner)
            .or_default()
            .push(effect);
        seq
    }

    /// 取当前生效（优先级最高、同优先级取最新）的服务实例。
    pub fn service<T: Any + Send + Sync>(&self) -> Option<Arc<T>> {
        let g = self.inner.services.read().unwrap();
        let list = g.get(&TypeId::of::<T>())?;
        let best = list.iter().max_by_key(|e| (e.priority, e.seq))?;
        best.value.clone().downcast::<T>().ok()
    }

    /// 取同一服务的整条链，按优先级从高到低。
    ///
    /// 「包裹/修改而不是替换」就是靠这个实现的：高优先级提供者在自己的实现里
    /// 调用 `services::<T>()[1..]`，把调用转发给被它遮蔽的那一层。
    pub fn services<T: Any + Send + Sync>(&self) -> Vec<Arc<T>> {
        let g = self.inner.services.read().unwrap();
        let Some(list) = g.get(&TypeId::of::<T>()) else {
            return Vec::new();
        };
        let mut sorted: Vec<&ServiceEntry> = list.iter().collect();
        sorted.sort_by_key(|e| std::cmp::Reverse((e.priority, e.seq)));
        sorted
            .into_iter()
            .filter_map(|e| e.value.clone().downcast::<T>().ok())
            .collect()
    }

    /// 活句柄：始终解析到"此刻生效的那个"服务，用于临界区外的长期持有。
    pub fn service_handle<T: Any + Send + Sync>(&self) -> ServiceHandle<T> {
        ServiceHandle {
            ctx: self.clone(),
            _marker: PhantomData,
        }
    }

    /// 订阅事件。返回的 effect 已挂到当前插件名下，插件停用时自动退订。
    pub fn on<E: 'static>(&self, handler: impl Fn(&E) + Send + Sync + 'static) {
        let owner = self.owner_now();
        let seq = self.inner.next_seq();
        let effect = self.inner.events.subscribe::<E>(seq, handler);
        self.inner
            .effects
            .lock()
            .unwrap()
            .entry(owner)
            .or_default()
            .push(effect);
    }

    /// 广播事件。
    pub fn emit<E: 'static>(&self, event: E) {
        self.inner.events.emit(&event);
    }

    /// Publish an explicit diagnostic without letting observer panics fail the command.
    ///
    /// Ordinary [`Context::emit`] keeps its existing panic behavior. The bus stops
    /// this publication at a panicking observer; writer subscribers should also
    /// isolate their own work so subsequent diagnostic observers can still run.
    pub fn diagnostic(&self, mut record: crate::DiagnosticRecord) {
        record.mark_emitted();
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.inner.events.emit(&record);
        }));
    }

    /// Register cleanup under the current owner, run once in reverse effect order.
    pub fn on_dispose(&self, cleanup: impl FnOnce() + Send + Sync + 'static) {
        let owner = self.owner_now();
        self.inner
            .effects
            .lock()
            .unwrap()
            .entry(owner)
            .or_default()
            .push(Effect::new(cleanup));
    }

    /// 全部服务清单，用于诊断面板与测试。
    pub fn registry_snapshot(&self) -> Vec<(ServiceKey, PluginId, i32)> {
        let g = self.inner.services.read().unwrap();
        let mut out = Vec::new();
        for list in g.values() {
            for e in list {
                out.push((e.service, e.owner.clone(), e.priority));
            }
        }
        out.sort();
        out
    }

    /// 某个插件名下已登记的 effect 数量。
    pub fn effect_count(&self, owner: &str) -> usize {
        self.inner
            .effects
            .lock()
            .unwrap()
            .get(owner)
            .map(Vec::len)
            .unwrap_or(0)
    }

    pub fn plugin_states(&self) -> HashMap<PluginId, PluginState> {
        self.inner.states.read().unwrap().clone()
    }

    pub fn set_plugin_state(&self, id: &str, state: PluginState) {
        self.inner
            .states
            .write()
            .unwrap()
            .insert(id.to_string(), state);
    }

    /// 逆序撤销某插件的全部注册。
    pub fn dispose_owner(&self, owner: &str) {
        let drained = {
            let mut g = self.inner.effects.lock().unwrap();
            g.remove(owner).unwrap_or_default()
        };
        for e in drained.into_iter().rev() {
            e.dispose();
        }
    }

    /// 事件订阅总数（诊断用）。
    pub fn subscription_count(&self) -> usize {
        self.inner.events.len()
    }
}

/// 活服务句柄。
pub struct ServiceHandle<T: Any + Send + Sync> {
    ctx: Context,
    _marker: PhantomData<fn() -> T>,
}

impl<T: Any + Send + Sync> ServiceHandle<T> {
    pub fn current(&self) -> Option<Arc<T>> {
        self.ctx.service::<T>()
    }

    pub fn is_available(&self) -> bool {
        self.current().is_some()
    }
}

impl<T: Any + Send + Sync> Clone for ServiceHandle<T> {
    fn clone(&self) -> Self {
        ServiceHandle {
            ctx: self.ctx.clone(),
            _marker: PhantomData,
        }
    }
}

// ---------------------------------------------------------------------------
// 装载器
// ---------------------------------------------------------------------------

/// 插件装载器：把 `PluginSpec` 列表变成跑起来的插件图。
pub struct Kernel {
    ctx: Context,
    factories: RwLock<HashMap<String, PluginFactory>>,
    plugins: RwLock<HashMap<PluginId, Arc<dyn Plugin>>>,
    specs: RwLock<Vec<PluginSpec>>,
    shut_down: AtomicBool,
}

impl Default for Kernel {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for Kernel {
    fn drop(&mut self) {
        self.shutdown();
    }
}

impl Kernel {
    pub fn new() -> Self {
        Kernel {
            ctx: Context::new(),
            factories: RwLock::new(HashMap::new()),
            plugins: RwLock::new(HashMap::new()),
            specs: RwLock::new(Vec::new()),
            shut_down: AtomicBool::new(false),
        }
    }

    pub fn context(&self) -> &Context {
        &self.ctx
    }

    pub fn register_factory(
        &self,
        name: &str,
        factory: impl Fn() -> Arc<dyn Plugin> + Send + Sync + 'static,
    ) {
        self.factories
            .write()
            .unwrap()
            .insert(name.to_string(), Arc::new(factory));
    }

    pub fn plugin(&self, id: &str) -> Option<Arc<dyn Plugin>> {
        self.plugins.read().unwrap().get(id).cloned()
    }

    pub fn loaded_ids(&self) -> Vec<PluginId> {
        let mut ids: Vec<PluginId> = self.plugins.read().unwrap().keys().cloned().collect();
        ids.sort();
        ids
    }

    pub fn specs(&self) -> Vec<PluginSpec> {
        self.specs.read().unwrap().clone()
    }

    fn lifecycle_outcome(&self, event: &str, plugin: Option<&str>, outcome: &str) {
        let mut record = crate::DiagnosticRecord::new("kernel", event, crate::Severity::Info)
            .with_field("outcome", outcome);
        if let Some(plugin) = plugin {
            record = record.with_field("plugin_id", plugin);
        }
        self.ctx.diagnostic(record);
    }

    fn lifecycle_failure(&self, event: &str, plugin_hint: Option<&str>, error: &KernelError) {
        let (category, plugin, rejected) = match error {
            KernelError::UnknownFactory(_) => ("factory", plugin_hint, false),
            KernelError::Cycle(_) => ("dependency", None, false),
            KernelError::MissingDependency { plugin, .. } => {
                ("dependency", Some(plugin.as_str()), false)
            }
            KernelError::Config { plugin, .. } => ("config", Some(plugin.as_str()), false),
            KernelError::Apply { plugin, .. } => ("apply", Some(plugin.as_str()), false),
            // An unloaded target is caller text, not an actual registered plugin identity.
            KernelError::NotLoaded(_) => ("not_loaded", None, true),
            KernelError::Required(plugin) => ("required", Some(plugin.as_str()), true),
        };
        let level = if rejected {
            crate::Severity::Warn
        } else {
            crate::Severity::Error
        };
        let mut record = crate::DiagnosticRecord::new("kernel", event, level)
            .with_field("outcome", if rejected { "rejected" } else { "failed" })
            .with_field("category", category);
        if let Some(plugin) = plugin {
            record = record.with_field("plugin_id", plugin);
        }
        self.ctx.diagnostic(record);
    }

    /// 按规格装载全部启用的插件。
    pub fn load(&self, specs: &[PluginSpec]) -> Result<(), KernelError> {
        for spec in specs {
            if spec.required && (!spec.enabled || !spec.when.iter().all(|p| p.evaluate())) {
                let error = KernelError::Required(spec.id.clone());
                self.lifecycle_failure("plugin.load", Some(&spec.id), &error);
                return Err(error);
            }
        }
        let active: Vec<PluginSpec> = specs
            .iter()
            .filter(|s| s.enabled && s.when.iter().all(|p| p.evaluate()))
            .cloned()
            .collect();
        self.shut_down.store(false, Ordering::SeqCst);
        // 记住**全部**规格（含被停用的），插件管理页需要把它们列出来
        *self.specs.write().unwrap() = specs.to_vec();

        // 1) 实例化
        let mut instances: HashMap<PluginId, Arc<dyn Plugin>> = HashMap::new();
        for spec in &active {
            let factory = { self.factories.read().unwrap().get(&spec.name).cloned() };
            let factory = factory.ok_or_else(|| {
                let error = KernelError::UnknownFactory(spec.name.clone());
                self.lifecycle_failure("plugin.load", Some(&spec.id), &error);
                error
            })?;
            let plugin = factory();
            instances.insert(spec.id.clone(), plugin);
        }

        // 2) 拓扑排序（依赖用插件 id 表达）
        let order = topological_order(&active, &instances).map_err(|error| {
            self.lifecycle_failure("plugin.load", None, &error);
            error
        })?;

        // 3) 逐个启动
        self.plugins.write().unwrap().extend(instances.clone());
        for id in &order {
            let spec = active
                .iter()
                .find(|s| &s.id == id)
                .expect("拓扑序里的 id 必须来自规格");
            let plugin = instances.get(id).expect("已实例化");
            self.ctx.set_plugin_state(id, PluginState::Starting);

            let mut cfg = plugin.default_config();
            merge_json(&mut cfg, &spec.config);
            if let Some(schema) = plugin.schema() {
                schema::validate(&schema, &cfg).map_err(|e| {
                    let error = KernelError::Config {
                        plugin: id.clone(),
                        reason: e.to_string(),
                    };
                    self.lifecycle_failure("plugin.apply", Some(id), &error);
                    error
                })?;
            }

            let result: PluginResult = self.ctx.with_owner(id, |ctx| plugin.apply(ctx, cfg));
            match result {
                Ok(()) => {
                    self.ctx.set_plugin_state(id, PluginState::Ready);
                    self.lifecycle_outcome("plugin.apply", Some(id), "succeeded");
                }
                Err(e) => {
                    self.ctx.set_plugin_state(id, PluginState::Failed);
                    self.ctx.dispose_owner(id);
                    let error = KernelError::Apply {
                        plugin: id.clone(),
                        reason: e.to_string(),
                    };
                    self.lifecycle_failure("plugin.apply", Some(id), &error);
                    return Err(error);
                }
            }
        }
        Ok(())
    }

    /// 停用插件：逆序撤销它的全部注册，并把状态置为 `Disabled`。
    ///
    /// 必需插件（`PluginSpec::required`）拒绝停用：这类插件提供的是启动器
    /// 的地基（版本扫描、设置解析、Java 运行时、本地内容……），停掉之后界面
    /// 还在但满屏「服务不可用」，属于比直接报错更糟的半残状态。
    pub fn disable(&self, id: &str) -> Result<(), KernelError> {
        let loaded = self.plugins.read().unwrap().contains_key(id);
        if !loaded {
            let error = KernelError::NotLoaded(id.to_string());
            self.lifecycle_failure("plugin.disable", Some(id), &error);
            return Err(error);
        }
        let required = self
            .specs
            .read()
            .map(|specs| specs.iter().any(|s| s.id == id && s.required))
            .unwrap_or(false);
        if required {
            let error = KernelError::Required(id.to_string());
            self.lifecycle_failure("plugin.disable", Some(id), &error);
            return Err(error);
        }
        self.ctx.dispose_owner(id);
        self.ctx.set_plugin_state(id, PluginState::Disabled);
        // 规格本身也要标成停用，否则插件管理页会显示"启用着但没跑"
        if let Ok(mut specs) = self.specs.write() {
            if let Some(spec) = specs.iter_mut().find(|s| s.id == id) {
                spec.enabled = false;
            }
        }
        self.lifecycle_outcome("plugin.disable", Some(id), "succeeded");
        Ok(())
    }

    /// 重新启用（等价于再次 apply）。
    pub fn enable(&self, id: &str) -> Result<(), KernelError> {
        let not_loaded = || {
            let error = KernelError::NotLoaded(id.to_string());
            self.lifecycle_failure("plugin.enable", Some(id), &error);
            error
        };
        let plugin = self.plugin(id).ok_or_else(not_loaded)?;
        let spec = self
            .specs()
            .into_iter()
            .find(|s| s.id == id)
            .ok_or_else(not_loaded)?;
        let mut cfg = plugin.default_config();
        merge_json(&mut cfg, &spec.config);
        if let Some(schema) = plugin.schema() {
            schema::validate(&schema, &cfg).map_err(|e| {
                let error = KernelError::Config {
                    plugin: id.to_string(),
                    reason: e.to_string(),
                };
                self.lifecycle_failure("plugin.enable", Some(id), &error);
                error
            })?;
        }
        self.shut_down.store(false, Ordering::SeqCst);
        self.ctx.set_plugin_state(id, PluginState::Starting);
        let result = self.ctx.with_owner(id, |ctx| plugin.apply(ctx, cfg));
        match result {
            Ok(()) => {
                self.ctx.set_plugin_state(id, PluginState::Ready);
                if let Ok(mut specs) = self.specs.write() {
                    if let Some(spec) = specs.iter_mut().find(|s| s.id == id) {
                        spec.enabled = true;
                    }
                }
                self.lifecycle_outcome("plugin.enable", Some(id), "succeeded");
                Ok(())
            }
            Err(e) => {
                self.ctx.set_plugin_state(id, PluginState::Failed);
                self.ctx.dispose_owner(id);
                let error = KernelError::Apply {
                    plugin: id.to_string(),
                    reason: e.to_string(),
                };
                self.lifecycle_failure("plugin.enable", Some(id), &error);
                Err(error)
            }
        }
    }

    /// 关闭：撤销所有插件的注册。
    pub fn shutdown(&self) {
        let publish = !self.shut_down.swap(true, Ordering::SeqCst);
        if publish {
            self.lifecycle_outcome("kernel.shutdown", None, "started");
        }
        let ids: Vec<PluginId> = {
            let g = self.plugins.read().unwrap();
            g.keys().cloned().collect()
        };
        // 逆拓扑序撤销更贴近真实依赖方向；这里按装载顺序反向即可。
        let order = self.specs().into_iter().map(|s| s.id).collect::<Vec<_>>();
        for id in order.into_iter().rev() {
            self.ctx.dispose_owner(&id);
            if ids.contains(&id) {
                self.ctx.set_plugin_state(&id, PluginState::Disabled);
            }
        }
        // Bootstrap can register owners before specs exist (or before load fails).
        // Snapshot owner names, then invoke cleanup only after releasing the lock.
        let mut remaining: Vec<PluginId> = {
            self.ctx
                .inner
                .effects
                .lock()
                .unwrap()
                .keys()
                .filter(|owner| !owner.is_empty())
                .cloned()
                .collect()
        };
        remaining.sort();
        for owner in remaining.into_iter().rev() {
            self.ctx.dispose_owner(&owner);
        }
        if publish {
            self.lifecycle_outcome("kernel.shutdown", None, "succeeded");
        }
        self.ctx.dispose_owner("");
    }
}

fn topological_order(
    specs: &[PluginSpec],
    instances: &HashMap<PluginId, Arc<dyn Plugin>>,
) -> Result<Vec<PluginId>, KernelError> {
    let ids: Vec<PluginId> = specs.iter().map(|s| s.id.clone()).collect();
    let mut deps: HashMap<PluginId, Vec<PluginId>> = HashMap::new();
    for spec in specs {
        let plugin = instances.get(&spec.id).expect("已实例化");
        let mut list = Vec::new();
        for dep in plugin.inject() {
            if !ids.iter().any(|i| i == dep) {
                return Err(KernelError::MissingDependency {
                    plugin: spec.id.clone(),
                    dep: dep.to_string(),
                });
            }
            list.push(dep.to_string());
        }
        deps.insert(spec.id.clone(), list);
    }

    let mut order: Vec<PluginId> = Vec::new();
    let mut remaining: Vec<PluginId> = ids.clone();
    while !remaining.is_empty() {
        let ready: Vec<PluginId> = remaining
            .iter()
            .filter(|id| {
                deps.get(*id)
                    .map(|d| d.iter().all(|dep| order.contains(dep)))
                    .unwrap_or(true)
            })
            .cloned()
            .collect();
        if ready.is_empty() {
            let mut stuck = remaining.clone();
            stuck.sort();
            return Err(KernelError::Cycle(stuck.join(" -> ")));
        }
        for id in ready {
            remaining.retain(|r| r != &id);
            order.push(id);
        }
    }
    Ok(order)
}

/// 把 `overlay` 深合并进 `base`（对象递归合并，其余直接覆盖）。
pub fn merge_json(base: &mut Value, overlay: &Value) {
    match (base, overlay) {
        (Value::Object(b), Value::Object(o)) => {
            for (k, v) in o {
                match b.get_mut(k) {
                    Some(existing) => merge_json(existing, v),
                    None => {
                        b.insert(k.clone(), v.clone());
                    }
                }
            }
        }
        (b, o) => *b = o.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::Plugin;
    use std::sync::atomic::AtomicUsize;
    use std::sync::Mutex;

    #[derive(Debug, Clone, PartialEq)]
    struct Greeting(pub String);

    struct SettingsSvc {
        pub locale: &'static str,
    }

    struct L10n {
        hits: Arc<AtomicUsize>,
        order: Arc<Mutex<Vec<&'static str>>>,
    }

    impl Plugin for L10n {
        fn id(&self) -> &str {
            "l10n"
        }
        fn apply(&self, ctx: &Context, _cfg: Value) -> PluginResult {
            self.order.lock().unwrap().push("l10n");
            ctx.provide("l10n", SettingsSvc { locale: "zh-CN" }, 0);
            let hits = Arc::clone(&self.hits);
            ctx.on::<Greeting>(move |g| {
                let _ = &g.0;
                hits.fetch_add(1, Ordering::SeqCst);
            });
            Ok(())
        }
    }

    struct Ui {
        seen: Arc<Mutex<Option<&'static str>>>,
    }

    impl Plugin for Ui {
        fn id(&self) -> &str {
            "ui"
        }
        fn inject(&self) -> &'static [&'static str] {
            &["l10n"]
        }
        fn apply(&self, ctx: &Context, _cfg: Value) -> PluginResult {
            let svc = ctx.service::<SettingsSvc>().expect("l10n 必须先启动");
            *self.seen.lock().unwrap() = Some(svc.locale);
            Ok(())
        }
    }

    struct BadPlugin;

    impl Plugin for BadPlugin {
        fn id(&self) -> &str {
            "bad"
        }
        fn schema(&self) -> Option<Value> {
            Some(serde_json::json!({
                "type": "object",
                "required": ["mirror"],
                "properties": { "mirror": { "type": "string" } }
            }))
        }
        fn apply(&self, _ctx: &Context, _cfg: Value) -> PluginResult {
            Ok(())
        }
    }

    #[test]
    fn services_are_shadowed_by_priority_and_restored_on_disable() {
        let ctx = Context::new();
        ctx.with_owner("core", |c| {
            c.provide("settings", SettingsSvc { locale: "zh-CN" }, 0);
        });
        assert_eq!(ctx.service::<SettingsSvc>().unwrap().locale, "zh-CN");

        ctx.with_owner("plugin", |c| {
            c.provide("settings", SettingsSvc { locale: "en-US" }, 10);
        });
        assert_eq!(ctx.service::<SettingsSvc>().unwrap().locale, "en-US");
        // 整条链仍然可查，「包裹」实现据此转发
        assert_eq!(ctx.services::<SettingsSvc>().len(), 2);

        ctx.dispose_owner("plugin");
        assert_eq!(ctx.service::<SettingsSvc>().unwrap().locale, "zh-CN");
    }

    #[test]
    fn root_service_outlives_ambient_owner_and_is_removed_by_kernel_cleanup() {
        struct RootService(Arc<AtomicUsize>);
        impl Drop for RootService {
            fn drop(&mut self) { self.0.fetch_add(1, Ordering::SeqCst); }
        }
        for explicit_shutdown in [false, true] {
            let kernel = Kernel::new();
            let retained = kernel.context().clone();
            let dropped = Arc::new(AtomicUsize::new(0));
            retained.with_owner("news", |ctx| {
                ctx.provide_root("root-service", RootService(dropped.clone()), 0);
                ctx.provide("ordinary-service", SettingsSvc { locale: "zh-CN" }, 0);
            });
            let registry = retained.registry_snapshot();
            assert!(registry.iter().any(|(key, owner, _)| *key == "root-service" && owner.is_empty()));
            assert!(registry.iter().any(|(key, owner, _)| *key == "ordinary-service" && owner == "news"));
            retained.dispose_owner("news");
            assert!(retained.service::<SettingsSvc>().is_none());
            assert!(retained.service::<RootService>().is_some());
            assert_eq!(dropped.load(Ordering::SeqCst), 0);
            if explicit_shutdown {
                kernel.shutdown();
                kernel.shutdown();
                assert!(retained.service::<RootService>().is_none());
                assert_eq!(dropped.load(Ordering::SeqCst), 1);
            }
            drop(kernel);
            assert!(retained.service::<RootService>().is_none());
            assert_eq!(dropped.load(Ordering::SeqCst), 1);
        }
    }

    #[test]
    fn service_handle_follows_shadowing() {
        let ctx = Context::new();
        let handle = ctx.service_handle::<SettingsSvc>();
        assert!(!handle.is_available());
        ctx.with_owner("core", |c| {
            c.provide("settings", SettingsSvc { locale: "zh-CN" }, 0);
        });
        assert_eq!(handle.current().unwrap().locale, "zh-CN");
        ctx.with_owner("override", |c| {
            c.provide("settings", SettingsSvc { locale: "ja-JP" }, 5);
        });
        assert_eq!(handle.current().unwrap().locale, "ja-JP");
    }

    #[test]
    fn disabling_plugin_removes_its_event_subscriptions() {
        let ctx = Context::new();
        let hits = Arc::new(AtomicUsize::new(0));
        let kernel = Kernel::new();
        kernel.register_factory("l10n", {
            let hits = Arc::clone(&hits);
            move || {
                Arc::new(L10n {
                    hits: Arc::clone(&hits),
                    order: Arc::new(Mutex::new(Vec::new())),
                }) as Arc<dyn Plugin>
            }
        });
        let _ = &ctx;

        // 用独立 kernel 验证装载与停用
        let k = kernel;
        k.load(&[PluginSpec::new("l10n")]).unwrap();
        k.context().emit(Greeting("hi".into()));
        assert_eq!(hits.load(Ordering::SeqCst), 1);
        k.disable("l10n").unwrap();
        k.context().emit(Greeting("hi".into()));
        assert_eq!(hits.load(Ordering::SeqCst), 1);
        assert_eq!(
            k.context().plugin_states().get("l10n"),
            Some(&PluginState::Disabled)
        );
    }

    #[test]
    fn required_plugin_cannot_be_disabled() {
        let kernel = Kernel::new();
        kernel.register_factory("l10n", || {
            Arc::new(L10n {
                hits: Arc::new(AtomicUsize::new(0)),
                order: Arc::new(Mutex::new(Vec::new())),
            }) as Arc<dyn Plugin>
        });
        kernel.load(&[PluginSpec::new("l10n").required()]).unwrap();

        // 先记下状态：被拒绝的停用不能有副作用（不能先拆服务再报错）。
        let err = kernel.disable("l10n").unwrap_err();
        assert!(
            err.to_string().contains("必需插件"),
            "错误信息要说清为什么不能关：{err}"
        );
        assert_eq!(
            kernel.context().plugin_states().get("l10n"),
            Some(&PluginState::Ready)
        );

        // 没有 required 标记的插件照旧可以关。
        let plain = Kernel::new();
        plain.register_factory("l10n", || {
            Arc::new(L10n {
                hits: Arc::new(AtomicUsize::new(0)),
                order: Arc::new(Mutex::new(Vec::new())),
            }) as Arc<dyn Plugin>
        });
        plain.load(&[PluginSpec::new("l10n")]).unwrap();
        plain.disable("l10n").unwrap();
    }

    #[test]
    fn topology_respects_inject() {
        let seen = Arc::new(Mutex::new(None));
        let kernel = Kernel::new();
        let order = Arc::new(Mutex::new(Vec::new()));
        kernel.register_factory("l10n", {
            let order = Arc::clone(&order);
            move || {
                Arc::new(L10n {
                    hits: Arc::new(AtomicUsize::new(0)),
                    order: Arc::clone(&order),
                }) as Arc<dyn Plugin>
            }
        });
        kernel.register_factory("ui", {
            let seen = Arc::clone(&seen);
            move || {
                Arc::new(Ui {
                    seen: Arc::clone(&seen),
                }) as Arc<dyn Plugin>
            }
        });
        // 故意把 ui 排在前面，拓扑排序必须把它挪到 l10n 之后
        kernel
            .load(&[PluginSpec::new("ui"), PluginSpec::new("l10n")])
            .unwrap();
        assert_eq!(*order.lock().unwrap(), vec!["l10n"]);
        assert_eq!(*seen.lock().unwrap(), Some("zh-CN"));
    }

    #[test]
    fn missing_dependency_is_reported() {
        let kernel = Kernel::new();
        kernel.register_factory("ui", || {
            Arc::new(Ui {
                seen: Arc::new(Mutex::new(None)),
            }) as Arc<dyn Plugin>
        });
        let err = kernel.load(&[PluginSpec::new("ui")]).unwrap_err();
        assert!(matches!(err, KernelError::MissingDependency { .. }));
    }

    #[test]
    fn dependency_cycle_is_detected() {
        struct A;
        struct B;
        impl Plugin for A {
            fn id(&self) -> &str {
                "a"
            }
            fn inject(&self) -> &'static [&'static str] {
                &["b"]
            }
            fn apply(&self, _c: &Context, _cfg: Value) -> PluginResult {
                Ok(())
            }
        }
        impl Plugin for B {
            fn id(&self) -> &str {
                "b"
            }
            fn inject(&self) -> &'static [&'static str] {
                &["a"]
            }
            fn apply(&self, _c: &Context, _cfg: Value) -> PluginResult {
                Ok(())
            }
        }
        let kernel = Kernel::new();
        kernel.register_factory("a", || Arc::new(A) as Arc<dyn Plugin>);
        kernel.register_factory("b", || Arc::new(B) as Arc<dyn Plugin>);
        let err = kernel
            .load(&[PluginSpec::new("a"), PluginSpec::new("b")])
            .unwrap_err();
        assert!(matches!(err, KernelError::Cycle(_)));
    }

    #[test]
    fn config_is_validated_against_schema() {
        let kernel = Kernel::new();
        kernel.register_factory("bad", || Arc::new(BadPlugin) as Arc<dyn Plugin>);
        let err = kernel.load(&[PluginSpec::new("bad")]).unwrap_err();
        assert!(matches!(err, KernelError::Config { .. }), "got {err:?}");

        let ok = kernel.load(&[
            PluginSpec::new("bad").with_config(serde_json::json!({ "mirror": "bmclapi" }))
        ]);
        assert!(ok.is_ok(), "got {ok:?}");
    }

    #[test]
    fn disabled_spec_is_not_loaded() {
        let kernel = Kernel::new();
        kernel.register_factory("l10n", || {
            Arc::new(L10n {
                hits: Arc::new(AtomicUsize::new(0)),
                order: Arc::new(Mutex::new(Vec::new())),
            }) as Arc<dyn Plugin>
        });
        kernel.load(&[PluginSpec::new("l10n").disabled()]).unwrap();
        assert!(kernel.loaded_ids().is_empty());
        assert!(kernel.context().service::<SettingsSvc>().is_none());
    }

    #[test]
    fn required_load_rejection_preserves_existing_kernel_before_any_factory_runs() {
        let kernel = Kernel::new();
        let hits = Arc::new(AtomicUsize::new(0));
        kernel.register_factory("l10n", {
            let hits = hits.clone();
            move || {
                hits.fetch_add(1, Ordering::SeqCst);
                Arc::new(L10n {
                    hits: Arc::new(AtomicUsize::new(0)),
                    order: Arc::new(Mutex::new(Vec::new())),
                }) as Arc<dyn Plugin>
            }
        });
        kernel.load(&[PluginSpec::new("l10n")]).unwrap();
        let original_specs = serde_json::to_value(kernel.specs()).unwrap();
        let original_states = kernel.context().plugin_states();
        let original_registry = kernel.context().registry_snapshot();
        let original_effects = kernel.context().effect_count("l10n");
        let original_service = kernel.context().service::<SettingsSvc>().unwrap();
        let mut conditional = PluginSpec::new("l10n").with_id("required").required();
        conditional.when.push(crate::Predicate::Platform {
            platform: "never-a-platform".into(),
        });
        for invalid in [
            PluginSpec::new("l10n")
                .with_id("required")
                .required()
                .disabled(),
            conditional,
        ] {
            assert!(matches!(
                kernel.load(&[PluginSpec::new("l10n"), invalid]),
                Err(KernelError::Required(id)) if id == "required"
            ));
            assert_eq!(hits.load(Ordering::SeqCst), 1);
            assert_eq!(
                serde_json::to_value(kernel.specs()).unwrap(),
                original_specs
            );
            assert_eq!(kernel.context().plugin_states(), original_states);
            assert_eq!(kernel.context().registry_snapshot(), original_registry);
            assert_eq!(kernel.context().effect_count("l10n"), original_effects);
            assert!(Arc::ptr_eq(
                &kernel.context().service::<SettingsSvc>().unwrap(),
                &original_service
            ));
        }
    }

    #[test]
    fn optional_conditions_still_filter_specs_and_true_required_conditions_load() {
        let kernel = Kernel::new();
        kernel.register_factory("l10n", || {
            Arc::new(L10n {
                hits: Arc::new(AtomicUsize::new(0)),
                order: Arc::new(Mutex::new(Vec::new())),
            }) as Arc<dyn Plugin>
        });
        let mut skipped = PluginSpec::new("missing");
        skipped.when.push(crate::Predicate::Platform {
            platform: "never-a-platform".into(),
        });
        let mut required = PluginSpec::new("l10n").required();
        required.when.push(crate::Predicate::Platform {
            platform: std::env::consts::OS.into(),
        });
        kernel.load(&[skipped, required]).unwrap();
        assert_eq!(kernel.loaded_ids(), vec!["l10n"]);
        assert_eq!(kernel.specs().len(), 2);
    }

    #[test]
    fn diagnostics_publish_explicit_safe_fields_and_correlations() {
        use crate::{DiagnosticRecord, Severity};
        let ctx = Context::new();
        let records = Arc::new(Mutex::new(Vec::new()));
        ctx.on::<DiagnosticRecord>({
            let records = records.clone();
            move |record| records.lock().unwrap().push(record.clone())
        });
        let before = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        ctx.with_owner("not-the-component", |ctx| {
            ctx.diagnostic(
                DiagnosticRecord::new("downloads", "batch.finished", Severity::Info)
                    .with_field("completed", 3_u64)
                    .with_operation_id("operation-1")
                    .with_session_id("session-1")
                    .with_job_id("job-1")
                    .with_batch_id("batch-1"),
            );
        });
        let after = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        let records = records.lock().unwrap();
        assert_eq!(records.len(), 1);
        let record = &records[0];
        assert_eq!(record.component, "downloads");
        assert_eq!(record.event, "batch.finished");
        assert_eq!(record.level, Severity::Info);
        assert_eq!(
            record.fields,
            std::collections::BTreeMap::from([("completed".into(), serde_json::json!(3))])
        );
        assert_eq!(record.operation_id.as_deref(), Some("operation-1"));
        assert_eq!(record.session_id.as_deref(), Some("session-1"));
        assert_eq!(record.job_id.as_deref(), Some("job-1"));
        assert_eq!(record.batch_id.as_deref(), Some("batch-1"));
        assert!((before..=after).contains(&record.timestamp_at_emission_ms));
        assert_eq!(serde_json::to_value(record).unwrap()["level"], "info");
        drop(records);
        ctx.dispose_owner("");
    }

    #[test]
    fn diagnostic_observer_panics_are_isolated_but_ordinary_event_panics_propagate() {
        use crate::{DiagnosticRecord, Severity};
        let ctx = Context::new();
        ctx.on::<DiagnosticRecord>(|_| panic!("broken diagnostic observer"));
        ctx.diagnostic(DiagnosticRecord::new("test", "safe", Severity::Warn));
        ctx.on::<Greeting>(|_| panic!("ordinary observer"));
        assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            ctx.emit(Greeting("hello".into()));
        }))
        .is_err());
        ctx.dispose_owner("");
    }

    #[test]
    fn on_dispose_runs_once_in_reverse_registration_order_outside_locks() {
        let ctx = Context::new();
        let order = Arc::new(Mutex::new(Vec::new()));
        ctx.with_owner("worker", |ctx| {
            for i in 0..3 {
                let order = order.clone();
                let callback_ctx = ctx.clone();
                ctx.on_dispose(move || {
                    assert_eq!(callback_ctx.effect_count("worker"), 0);
                    callback_ctx.set_plugin_state("cleanup", PluginState::Disabled);
                    order.lock().unwrap().push(i);
                });
            }
        });
        assert_eq!(ctx.effect_count("worker"), 3);
        assert!(order.lock().unwrap().is_empty());
        ctx.dispose_owner("worker");
        ctx.dispose_owner("worker");
        assert_eq!(*order.lock().unwrap(), vec![2, 1, 0]);
    }

    #[test]
    fn kernel_drop_disposes_effects_even_when_context_is_retained() {
        let retained;
        let calls = Arc::new(AtomicUsize::new(0));
        {
            let kernel = Kernel::new();
            kernel.register_factory("l10n", || {
                Arc::new(L10n {
                    hits: Arc::new(AtomicUsize::new(0)),
                    order: Arc::new(Mutex::new(Vec::new())),
                }) as Arc<dyn Plugin>
            });
            kernel.load(&[PluginSpec::new("l10n")]).unwrap();
            retained = kernel.context().clone();
            retained.with_owner("l10n", |ctx| {
                let calls = calls.clone();
                ctx.on_dispose(move || {
                    calls.fetch_add(1, Ordering::SeqCst);
                });
            });
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(retained.service::<SettingsSvc>().is_none());
        assert_eq!(retained.effect_count("l10n"), 0);
    }

    fn capture_diagnostics(ctx: &Context) -> Arc<Mutex<Vec<crate::DiagnosticRecord>>> {
        let records = Arc::new(Mutex::new(Vec::new()));
        let sink = records.clone();
        let callback_ctx = ctx.clone();
        ctx.on::<crate::DiagnosticRecord>(move |record| {
            // Lifecycle observers must be able to query state/services without deadlock.
            let _ = callback_ctx.registry_snapshot();
            let _ = callback_ctx.plugin_states();
            sink.lock().unwrap().push(record.clone());
        });
        records
    }

    fn diagnostic_summaries(records: &Arc<Mutex<Vec<crate::DiagnosticRecord>>>) -> Value {
        Value::Array(
            records
                .lock()
                .unwrap()
                .iter()
                .map(|record| {
                    assert_eq!(record.component, "kernel");
                    assert!(record.timestamp_at_emission_ms > 0);
                    serde_json::json!({
                        "event": record.event,
                        "level": record.level,
                        "fields": record.fields,
                    })
                })
                .collect(),
        )
    }

    struct DiagnosticPlugin {
        dependencies: &'static [&'static str],
        attempts: Arc<AtomicUsize>,
        fail_on: usize,
        invalid_config: Arc<std::sync::atomic::AtomicBool>,
    }

    impl Default for DiagnosticPlugin {
        fn default() -> Self {
            Self {
                dependencies: &[],
                attempts: Arc::new(AtomicUsize::new(0)),
                fail_on: 0,
                invalid_config: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            }
        }
    }

    impl Plugin for DiagnosticPlugin {
        fn id(&self) -> &str {
            "worker"
        }
        fn inject(&self) -> &'static [&'static str] {
            self.dependencies
        }
        fn schema(&self) -> Option<Value> {
            self.invalid_config.load(Ordering::SeqCst).then(|| {
                serde_json::json!({
                    "type": "object", "required": ["private-schema-field"]
                })
            })
        }
        fn apply(&self, ctx: &Context, _cfg: Value) -> PluginResult {
            ctx.provide("worker", SettingsSvc { locale: "en-US" }, 0);
            ctx.on::<Greeting>(|_| {});
            let attempt = self.attempts.fetch_add(1, Ordering::SeqCst) + 1;
            if attempt == self.fail_on {
                Err("private-token=https://user:password@example.invalid/?token=secret".into())
            } else {
                Ok(())
            }
        }
    }

    #[test]
    fn lifecycle_records_are_finite_and_shutdown_is_idempotent() {
        let kernel = Kernel::new();
        let records = capture_diagnostics(kernel.context());
        kernel.register_factory("worker", || Arc::new(DiagnosticPlugin::default()));
        kernel
            .load(&[
                PluginSpec::new("worker"),
                PluginSpec::new("skipped").disabled(),
            ])
            .unwrap();
        kernel.disable("worker").unwrap();
        kernel.enable("worker").unwrap();
        kernel.shutdown();
        let after_shutdown = capture_diagnostics(kernel.context());
        kernel.shutdown();
        drop(kernel);
        assert_eq!(diagnostic_summaries(&after_shutdown), serde_json::json!([]));
        assert_eq!(
            diagnostic_summaries(&records),
            serde_json::json!([
                {"event":"plugin.apply", "level":"info", "fields":{"plugin_id":"worker", "outcome":"succeeded"}},
                {"event":"plugin.disable", "level":"info", "fields":{"plugin_id":"worker", "outcome":"succeeded"}},
                {"event":"plugin.enable", "level":"info", "fields":{"plugin_id":"worker", "outcome":"succeeded"}},
                {"event":"kernel.shutdown", "level":"info", "fields":{"outcome":"started"}},
                {"event":"kernel.shutdown", "level":"info", "fields":{"outcome":"succeeded"}}
            ])
        );
    }

    #[test]
    fn lifecycle_shutdown_resets_after_subsequent_load_or_enable() {
        for reload in [false, true] {
            let kernel = Kernel::new();
            kernel.register_factory("worker", || Arc::new(DiagnosticPlugin::default()));
            kernel.load(&[PluginSpec::new("worker")]).unwrap();
            kernel.shutdown();
            let records = capture_diagnostics(kernel.context());
            if reload {
                kernel.load(&[PluginSpec::new("worker")]).unwrap();
            } else {
                kernel.enable("worker").unwrap();
            }
            kernel.shutdown();
            assert_eq!(
                diagnostic_summaries(&records),
                serde_json::json!([
                    {"event":if reload {"plugin.apply"} else {"plugin.enable"}, "level":"info", "fields":{"plugin_id":"worker", "outcome":"succeeded"}},
                    {"event":"kernel.shutdown", "level":"info", "fields":{"outcome":"started"}},
                    {"event":"kernel.shutdown", "level":"info", "fields":{"outcome":"succeeded"}}
                ])
            );
        }
    }

    #[test]
    fn lifecycle_drop_cleans_bootstrap_owners_even_before_load() {
        let retained;
        let order = Arc::new(Mutex::new(Vec::new()));
        {
            let kernel = Kernel::new();
            retained = kernel.context().clone();
            retained.with_owner("bootstrap", |ctx| {
                ctx.provide("bootstrap", SettingsSvc { locale: "en-US" }, 0);
                for i in 0..2 {
                    let order = order.clone();
                    let callback_ctx = ctx.clone();
                    ctx.on_dispose(move || {
                        assert_eq!(callback_ctx.effect_count("bootstrap"), 0);
                        order.lock().unwrap().push(i);
                    });
                }
            });
        }
        assert_eq!(*order.lock().unwrap(), vec![1, 0]);
        assert!(retained.service::<SettingsSvc>().is_none());
        assert_eq!(retained.effect_count("bootstrap"), 0);
    }

    #[test]
    fn lifecycle_factory_graph_and_config_failures_emit_one_safe_record() {
        for (case, event, category, plugin_id) in [
            ("factory", "plugin.load", "factory", Some("requested")),
            ("missing", "plugin.load", "dependency", Some("requested")),
            ("cycle", "plugin.load", "dependency", None),
            ("config", "plugin.apply", "config", Some("requested")),
        ] {
            let kernel = Kernel::new();
            let records = capture_diagnostics(kernel.context());
            if case != "factory" {
                let case = case.to_string();
                kernel.register_factory("worker", move || {
                    Arc::new(DiagnosticPlugin {
                        dependencies: match case.as_str() {
                            "missing" => &["absent"],
                            "cycle" => &["requested"],
                            _ => &[],
                        },
                        invalid_config: Arc::new(std::sync::atomic::AtomicBool::new(
                            case == "config",
                        )),
                        ..Default::default()
                    })
                });
            }
            let err = kernel
                .load(&[PluginSpec::new("worker")
                    .with_id("requested")
                    .with_config(serde_json::json!({"private-config": "secret"}))])
                .unwrap_err();
            assert!(match case {
                "factory" => matches!(err, KernelError::UnknownFactory(_)),
                "missing" => matches!(err, KernelError::MissingDependency { .. }),
                "cycle" => matches!(err, KernelError::Cycle(_)),
                "config" => matches!(err, KernelError::Config { .. }),
                _ => unreachable!(),
            });
            assert!(kernel.context().service::<SettingsSvc>().is_none());
            let mut fields = serde_json::json!({"outcome":"failed", "category":category});
            if let Some(plugin_id) = plugin_id {
                fields["plugin_id"] = plugin_id.into();
            }
            assert_eq!(
                diagnostic_summaries(&records),
                serde_json::json!([
                    {"event":event, "level":"error", "fields":fields}
                ]),
                "case {case}"
            );
        }
    }

    #[test]
    fn lifecycle_initial_apply_failure_keeps_original_error_and_rolls_back() {
        let kernel = Kernel::new();
        let records = capture_diagnostics(kernel.context());
        kernel.register_factory("worker", || {
            Arc::new(DiagnosticPlugin {
                fail_on: 1,
                ..Default::default()
            })
        });
        let err = kernel.load(&[PluginSpec::new("worker")]).unwrap_err();
        assert!(matches!(err, KernelError::Apply { plugin, reason }
            if plugin == "worker" && reason.contains("private-token")));
        assert_eq!(
            kernel.context().plugin_states()["worker"],
            PluginState::Failed
        );
        assert!(kernel.context().service::<SettingsSvc>().is_none());
        assert_eq!(kernel.context().effect_count("worker"), 0);
        assert_eq!(
            diagnostic_summaries(&records),
            serde_json::json!([
                {"event":"plugin.apply", "level":"error", "fields":{"plugin_id":"worker", "outcome":"failed", "category":"apply"}}
            ])
        );
    }

    #[test]
    fn lifecycle_reenable_failure_then_success_reports_each_outcome_once() {
        let kernel = Kernel::new();
        let records = capture_diagnostics(kernel.context());
        kernel.register_factory("worker", || {
            Arc::new(DiagnosticPlugin {
                fail_on: 2,
                ..Default::default()
            })
        });
        kernel.load(&[PluginSpec::new("worker")]).unwrap();
        kernel.disable("worker").unwrap();
        assert!(matches!(
            kernel.enable("worker"),
            Err(KernelError::Apply { .. })
        ));
        assert_eq!(
            kernel.context().plugin_states()["worker"],
            PluginState::Failed
        );
        assert!(kernel.context().service::<SettingsSvc>().is_none());
        assert_eq!(kernel.context().effect_count("worker"), 0);
        assert!(!kernel.specs()[0].enabled);
        kernel.enable("worker").unwrap();
        assert_eq!(
            kernel.context().plugin_states()["worker"],
            PluginState::Ready
        );
        assert!(kernel.context().service::<SettingsSvc>().is_some());
        assert!(kernel.specs()[0].enabled);
        assert_eq!(
            diagnostic_summaries(&records),
            serde_json::json!([
                {"event":"plugin.apply", "level":"info", "fields":{"plugin_id":"worker", "outcome":"succeeded"}},
                {"event":"plugin.disable", "level":"info", "fields":{"plugin_id":"worker", "outcome":"succeeded"}},
                {"event":"plugin.enable", "level":"error", "fields":{"plugin_id":"worker", "outcome":"failed", "category":"apply"}},
                {"event":"plugin.enable", "level":"info", "fields":{"plugin_id":"worker", "outcome":"succeeded"}}
            ])
        );
    }

    #[test]
    fn lifecycle_reenable_config_failure_preserves_existing_disabled_state() {
        let kernel = Kernel::new();
        let records = capture_diagnostics(kernel.context());
        let invalid = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let attempts = Arc::new(AtomicUsize::new(0));
        kernel.register_factory("worker", {
            let invalid = invalid.clone();
            let attempts = attempts.clone();
            move || {
                Arc::new(DiagnosticPlugin {
                    invalid_config: invalid.clone(),
                    attempts: attempts.clone(),
                    ..Default::default()
                })
            }
        });
        kernel.load(&[PluginSpec::new("worker")]).unwrap();
        kernel.disable("worker").unwrap();
        invalid.store(true, Ordering::SeqCst);
        assert!(matches!(
            kernel.enable("worker"),
            Err(KernelError::Config { .. })
        ));
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
        assert_eq!(
            kernel.context().plugin_states()["worker"],
            PluginState::Disabled
        );
        assert!(!kernel.specs()[0].enabled);
        assert!(kernel.context().service::<SettingsSvc>().is_none());
        assert_eq!(
            diagnostic_summaries(&records),
            serde_json::json!([
                {"event":"plugin.apply", "level":"info", "fields":{"plugin_id":"worker", "outcome":"succeeded"}},
                {"event":"plugin.disable", "level":"info", "fields":{"plugin_id":"worker", "outcome":"succeeded"}},
                {"event":"plugin.enable", "level":"error", "fields":{"plugin_id":"worker", "outcome":"failed", "category":"config"}}
            ])
        );
    }

    #[test]
    fn lifecycle_required_disable_rejection_preserves_service_and_effects() {
        let kernel = Kernel::new();
        let records = capture_diagnostics(kernel.context());
        kernel.register_factory("worker", || Arc::new(DiagnosticPlugin::default()));
        kernel
            .load(&[PluginSpec::new("worker").required()])
            .unwrap();
        let service = kernel.context().service::<SettingsSvc>().unwrap();
        let registry = kernel.context().registry_snapshot();
        let effects = kernel.context().effect_count("worker");
        assert!(matches!(
            kernel.disable("worker"),
            Err(KernelError::Required(_))
        ));
        assert!(Arc::ptr_eq(
            &service,
            &kernel.context().service::<SettingsSvc>().unwrap()
        ));
        assert_eq!(kernel.context().registry_snapshot(), registry);
        assert_eq!(kernel.context().effect_count("worker"), effects);
        assert_eq!(
            kernel.context().plugin_states()["worker"],
            PluginState::Ready
        );
        assert!(kernel.specs()[0].enabled);
        assert_eq!(
            diagnostic_summaries(&records),
            serde_json::json!([
                {"event":"plugin.apply", "level":"info", "fields":{"plugin_id":"worker", "outcome":"succeeded"}},
                {"event":"plugin.disable", "level":"warn", "fields":{"plugin_id":"worker", "outcome":"rejected", "category":"required"}}
            ])
        );
    }

    #[test]
    fn lifecycle_not_loaded_and_required_load_rejections_emit_once() {
        let kernel = Kernel::new();
        let records = capture_diagnostics(kernel.context());
        assert!(matches!(
            kernel.disable("absent"),
            Err(KernelError::NotLoaded(_))
        ));
        assert!(matches!(
            kernel.enable("absent"),
            Err(KernelError::NotLoaded(_))
        ));
        assert!(matches!(
            kernel.load(&[PluginSpec::new("required").required().disabled()]),
            Err(KernelError::Required(_))
        ));
        assert_eq!(
            diagnostic_summaries(&records),
            serde_json::json!([
                {"event":"plugin.disable", "level":"warn", "fields":{"outcome":"rejected", "category":"not_loaded"}},
                {"event":"plugin.enable", "level":"warn", "fields":{"outcome":"rejected", "category":"not_loaded"}},
                {"event":"plugin.load", "level":"warn", "fields":{"plugin_id":"required", "outcome":"rejected", "category":"required"}}
            ])
        );
    }

    #[test]
    fn lifecycle_unknown_target_sentinel_is_not_a_plugin_identity() {
        let kernel = Kernel::new();
        let records = capture_diagnostics(kernel.context());
        let sentinel = "SENTINEL_TOKEN_3A";
        for error in [kernel.disable(sentinel).unwrap_err(), kernel.enable(sentinel).unwrap_err()] {
            assert!(matches!(&error, KernelError::NotLoaded(id) if id == sentinel));
            assert!(error.to_string().contains(sentinel));
        }
        let records = records.lock().unwrap().clone();
        assert_eq!(records.len(), 2);
        for record in records.iter() {
            assert_eq!(record.fields.get("category"), Some(&Value::String("not_loaded".into())));
            assert_eq!(record.fields.get("outcome"), Some(&Value::String("rejected".into())));
            assert!(!record.fields.contains_key("plugin_id"));
            assert!(!serde_json::to_string(record).unwrap().contains(sentinel));
        }
    }

    #[test]
    fn lifecycle_commands_succeed_despite_diagnostic_observer_panics() {
        let kernel = Kernel::new();
        let observed = Arc::new(AtomicUsize::new(0));
        kernel.context().on::<crate::DiagnosticRecord>({
            let observed = observed.clone();
            move |_| {
                observed.fetch_add(1, Ordering::SeqCst);
                panic!("broken logger");
            }
        });
        kernel.register_factory("worker", || Arc::new(DiagnosticPlugin::default()));
        kernel.load(&[PluginSpec::new("worker")]).unwrap();
        assert!(kernel.context().service::<SettingsSvc>().is_some());
        kernel.disable("worker").unwrap();
        assert!(kernel.context().service::<SettingsSvc>().is_none());
        kernel.enable("worker").unwrap();
        assert_eq!(
            kernel.context().plugin_states()["worker"],
            PluginState::Ready
        );
        kernel.shutdown();
        assert!(kernel.context().service::<SettingsSvc>().is_none());
        assert_eq!(observed.load(Ordering::SeqCst), 5);
    }

    #[test]
    fn deep_merge_json() {
        let mut base = serde_json::json!({ "a": { "b": 1, "c": 2 }, "d": 3 });
        merge_json(&mut base, &serde_json::json!({ "a": { "c": 9 }, "e": 5 }));
        assert_eq!(
            base,
            serde_json::json!({ "a": { "b": 1, "c": 9 }, "d": 3, "e": 5 })
        );
    }
}
