//! 内核上下文与插件装载器。
//!
//! [`Context`] 是插件能看到的全部世界：注册服务、订阅事件、查询状态。
//! 插件 `apply` 期间产生的每一次注册都会被内核记在这个插件名下，
//! 插件停用时**逆序撤销**——这就是"停用插件后界面与服务一起消失"的机制基础。

use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::marker::PhantomData;
use std::sync::atomic::{AtomicU64, Ordering};
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
        let owner = self.owner_now();
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
}

impl Default for Kernel {
    fn default() -> Self {
        Self::new()
    }
}

impl Kernel {
    pub fn new() -> Self {
        Kernel {
            ctx: Context::new(),
            factories: RwLock::new(HashMap::new()),
            plugins: RwLock::new(HashMap::new()),
            specs: RwLock::new(Vec::new()),
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

    /// 按规格装载全部启用的插件。
    pub fn load(&self, specs: &[PluginSpec]) -> Result<(), KernelError> {
        let active: Vec<PluginSpec> = specs
            .iter()
            .filter(|s| s.enabled && s.when.iter().all(|p| p.evaluate()))
            .cloned()
            .collect();
        // 记住**全部**规格（含被停用的），插件管理页需要把它们列出来
        *self.specs.write().unwrap() = specs.to_vec();

        // 1) 实例化
        let mut instances: HashMap<PluginId, Arc<dyn Plugin>> = HashMap::new();
        for spec in &active {
            let factory = self
                .factories
                .read()
                .unwrap()
                .get(&spec.name)
                .cloned()
                .ok_or_else(|| KernelError::UnknownFactory(spec.name.clone()))?;
            let plugin = factory();
            instances.insert(spec.id.clone(), plugin);
        }

        // 2) 拓扑排序（依赖用插件 id 表达）
        let order = topological_order(&active, &instances)?;

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
                schema::validate(&schema, &cfg).map_err(|e| KernelError::Config {
                    plugin: id.clone(),
                    reason: e.to_string(),
                })?;
            }

            let result: PluginResult = self.ctx.with_owner(id, |ctx| plugin.apply(ctx, cfg));
            match result {
                Ok(()) => self.ctx.set_plugin_state(id, PluginState::Ready),
                Err(e) => {
                    self.ctx.set_plugin_state(id, PluginState::Failed);
                    self.ctx.dispose_owner(id);
                    return Err(KernelError::Apply {
                        plugin: id.clone(),
                        reason: e.to_string(),
                    });
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
        if !self.plugins.read().unwrap().contains_key(id) {
            return Err(KernelError::NotLoaded(id.to_string()));
        }
        if self
            .specs
            .read()
            .map(|specs| specs.iter().any(|s| s.id == id && s.required))
            .unwrap_or(false)
        {
            return Err(KernelError::Required(id.to_string()));
        }
        self.ctx.dispose_owner(id);
        self.ctx.set_plugin_state(id, PluginState::Disabled);
        // 规格本身也要标成停用，否则插件管理页会显示"启用着但没跑"
        if let Ok(mut specs) = self.specs.write() {
            if let Some(spec) = specs.iter_mut().find(|s| s.id == id) {
                spec.enabled = false;
            }
        }
        Ok(())
    }

    /// 重新启用（等价于再次 apply）。
    pub fn enable(&self, id: &str) -> Result<(), KernelError> {
        let plugin = self
            .plugin(id)
            .ok_or_else(|| KernelError::NotLoaded(id.to_string()))?;
        let spec = self
            .specs()
            .into_iter()
            .find(|s| s.id == id)
            .ok_or_else(|| KernelError::NotLoaded(id.to_string()))?;
        let mut cfg = plugin.default_config();
        merge_json(&mut cfg, &spec.config);
        if let Some(schema) = plugin.schema() {
            schema::validate(&schema, &cfg).map_err(|e| KernelError::Config {
                plugin: id.to_string(),
                reason: e.to_string(),
            })?;
        }
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
                Ok(())
            }
            Err(e) => {
                self.ctx.set_plugin_state(id, PluginState::Failed);
                self.ctx.dispose_owner(id);
                Err(KernelError::Apply {
                    plugin: id.to_string(),
                    reason: e.to_string(),
                })
            }
        }
    }

    /// 关闭：撤销所有插件的注册。
    pub fn shutdown(&self) {
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
        kernel
            .load(&[PluginSpec::new("l10n").required()])
            .unwrap();

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
    fn deep_merge_json() {
        let mut base = serde_json::json!({ "a": { "b": 1, "c": 2 }, "d": 3 });
        merge_json(&mut base, &serde_json::json!({ "a": { "c": 9 }, "e": 5 }));
        assert_eq!(
            base,
            serde_json::json!({ "a": { "b": 1, "c": 9 }, "d": 3, "e": 5 })
        );
    }
}
