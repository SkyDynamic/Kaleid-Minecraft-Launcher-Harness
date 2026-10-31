//! 可回收副作用。
//!
//! 插件在 `apply` 期间注册的一切（服务、事件订阅、后台任务）都表现为一个 [`Effect`]。
//! `Effect` 一旦被丢弃，其内部闭包立即逆序执行，完成撤销。
//! 这是 Cordis「插件副作用随插件停用自动回滚」在 Rust 里的等价物。

use std::sync::Mutex;

/// 一个可撤销的注册动作。
pub struct Effect(Option<Box<dyn FnOnce() + Send + Sync>>);

impl Effect {
    /// 什么都不做。
    pub fn noop() -> Self {
        Effect(None)
    }

    /// 用给定闭包构造。
    pub fn new(f: impl FnOnce() + Send + Sync + 'static) -> Self {
        Effect(Some(Box::new(f)))
    }

    /// 立即撤销。
    pub fn dispose(mut self) {
        if let Some(f) = self.0.take() {
            f();
        }
    }

    /// 把多个 effect 合成一个，撤销时**逆序**执行。
    pub fn batch(effects: Vec<Effect>) -> Self {
        Effect::new(move || {
            for e in effects.into_iter().rev() {
                e.dispose();
            }
        })
    }
}

impl Drop for Effect {
    fn drop(&mut self) {
        if let Some(f) = self.0.take() {
            f();
        }
    }
}

impl std::fmt::Debug for Effect {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("Effect").field(&self.0.is_some()).finish()
    }
}

/// 收集 effect 的容器，drop 时逆序撤销。
#[derive(Default, Debug)]
pub struct EffectScope {
    effects: Mutex<Vec<Effect>>,
}

impl EffectScope {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&self, effect: Effect) {
        self.effects.lock().unwrap().push(effect);
    }

    /// 逆序撤销全部。
    pub fn clear(&self) {
        let drained: Vec<Effect> = {
            let mut g = self.effects.lock().unwrap();
            std::mem::take(&mut *g)
        };
        for e in drained.into_iter().rev() {
            e.dispose();
        }
    }

    pub fn len(&self) -> usize {
        self.effects.lock().unwrap().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl Drop for EffectScope {
    fn drop(&mut self) {
        self.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    #[test]
    fn effect_runs_on_drop() {
        let hit = Arc::new(AtomicUsize::new(0));
        {
            let h = hit.clone();
            let _e = Effect::new(move || {
                h.fetch_add(1, Ordering::SeqCst);
            });
            assert_eq!(hit.load(Ordering::SeqCst), 0);
        }
        assert_eq!(hit.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn explicit_dispose_is_idempotent_once() {
        let hit = Arc::new(AtomicUsize::new(0));
        let h = hit.clone();
        let e = Effect::new(move || {
            h.fetch_add(1, Ordering::SeqCst);
        });
        e.dispose();
        assert_eq!(hit.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn scope_disposes_in_reverse_order() {
        let order = Arc::new(Mutex::new(Vec::new()));
        let scope = EffectScope::new();
        for i in 0..4 {
            let o = order.clone();
            scope.push(Effect::new(move || o.lock().unwrap().push(i)));
        }
        scope.clear();
        assert_eq!(*order.lock().unwrap(), vec![3, 2, 1, 0]);
    }
}
