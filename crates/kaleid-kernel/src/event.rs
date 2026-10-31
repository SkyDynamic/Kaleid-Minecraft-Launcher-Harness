//! 类型化事件总线。
//!
//! 事件按**载荷类型**寻址（`TypeId`），不靠字符串名字，避免拼写错误变成运行期 bug。
//! 订阅返回 [`Effect`]，插件停用时自动退订。

use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use crate::effect::Effect;
use crate::plugin::PluginId;

type Handler = Arc<dyn Fn(&dyn Any) + Send + Sync>;

struct Subscriber {
    seq: u64,
    handler: Handler,
}

/// 进程内单例事件总线，由 `Context` 以 `Arc` 共享。
#[derive(Default)]
pub(crate) struct EventBus {
    subs: RwLock<HashMap<TypeId, Vec<Subscriber>>>,
}

impl EventBus {
    /// 订阅。`seq` 由内核分配，用于精确退订。
    pub(crate) fn subscribe<E: 'static>(
        self: &Arc<Self>,
        seq: u64,
        handler: impl Fn(&E) + Send + Sync + 'static,
    ) -> Effect {
        let wrapped: Handler = Arc::new(move |any: &dyn Any| {
            if let Some(e) = any.downcast_ref::<E>() {
                handler(e);
            }
        });
        {
            let mut g = self.subs.write().unwrap();
            g.entry(TypeId::of::<E>()).or_default().push(Subscriber {
                seq,
                handler: wrapped,
            });
        }
        let bus = Arc::clone(self);
        Effect::new(move || {
            if let Ok(mut g) = bus.subs.write() {
                if let Some(list) = g.get_mut(&TypeId::of::<E>()) {
                    list.retain(|s| s.seq != seq);
                }
            }
        })
    }

    /// 广播。**先取快照再回调**，避免处理器内部再订阅/退订造成死锁。
    pub(crate) fn emit<E: 'static>(&self, event: &E) {
        let snapshot: Vec<Handler> = {
            let g = self.subs.read().unwrap();
            g.get(&TypeId::of::<E>())
                .map(|list| list.iter().map(|s| s.handler.clone()).collect())
                .unwrap_or_default()
        };
        let any: &dyn Any = event;
        for h in snapshot {
            h(any);
        }
    }

    /// 当前订阅数量（测试与诊断用）。
    pub(crate) fn len(&self) -> usize {
        self.subs.read().unwrap().values().map(Vec::len).sum()
    }

    #[allow(dead_code)]
    pub(crate) fn owner_hint(&self) -> PluginId {
        String::new()
    }
}
