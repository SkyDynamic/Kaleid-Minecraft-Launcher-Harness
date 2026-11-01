//! 按主机记账的并发与限速。
//!
//! 一次原版安装要拿 4000+ 个文件，而各家源的性格完全不同：
//!
//! | 主机 | 实测性格 |
//! |---|---|
//! | `bmclapi2.bangbang93.com` | 重定向服务，响应头带 `X-RateLimit-Limit-short/Limit-long`，扛不住并发 |
//! | `piston-data.mojang.com` 等官方 CDN | 支持 Range，能吃满并发 |
//! | 镜像节点（cernet / ustc / sakura …） | 单节点带宽有限，打太狠会 429/超时 |
//!
//! 所以并发不能是"全局一个数字"，得按**目标主机**分别记账，并且在被限流时
//! **减半 + 冷却**、连续成功后慢慢加回来（AIMD）。这里只做记账，不做阻塞等待：
//! 调度器问一句"这台主机现在还能开连接吗"，不能就换别的文件先下。

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

/// 镜像中枢自己扛不了并发，先给它小一点的默认上限。
const BMCLAPI_HOST: &str = "bmclapi2.bangbang93.com";
const BMCLAPI_LIMIT: usize = 4;
/// 连续成功多少次才允许把预算加回 1。
const RECOVER_STREAK: usize = 20;
/// 429 没给 `Retry-After` 时的默认冷却。
const DEFAULT_COOLDOWN_MS: u64 = 3000;
/// 拿不到槽位时建议的重试间隔（调度器据此决定下一步等多久）。
pub const RETRY_HINT_MS: u64 = 200;

/// 一台主机的记账状态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostState {
    /// 并发上限（设置项与主机性格折算后的值）。
    pub limit: usize,
    /// 当前允许的并发（AIMD，`1..=limit`）。
    pub budget: usize,
    /// 正在跑的连接数。
    pub used: usize,
    /// 连续成功次数，够 [`RECOVER_STREAK`] 就把 `budget` 加一。
    pub ok_streak: usize,
    /// 冷却到哪个时刻（Unix 毫秒）。
    pub cooldown_until_ms: u64,
    /// 每分钟请求数上限；`None` 表示没有观测到明确配额，不主动掐。
    pub per_minute: Option<u32>,
    /// 当前分钟编号（`now_ms / 60_000`）。
    pub minute_id: u64,
    /// 本分钟已经发起的请求数。
    pub minute_used: u32,
}

impl HostState {
    fn new(limit: usize, per_minute: Option<u32>) -> Self {
        let limit = limit.max(1);
        HostState {
            limit,
            budget: limit,
            used: 0,
            ok_streak: 0,
            cooldown_until_ms: 0,
            per_minute,
            minute_id: 0,
            minute_used: 0,
        }
    }
}

/// 所有主机的记账表。`Arc` 共享给下载器即可。
pub struct HostLimits {
    /// 全局并发（设置项 `download_concurrency`），新主机默认用它当上限。
    concurrency: usize,
    inner: Mutex<HashMap<String, HostState>>,
}

impl HostLimits {
    pub fn for_concurrency(concurrency: usize) -> Self {
        HostLimits {
            concurrency: concurrency.max(1),
            inner: Mutex::new(HashMap::new()),
        }
    }

    /// 显式给某台主机定配额（测试与"从响应头学到配额"都用它）。
    pub fn set_per_minute(&self, host: &str, per_minute: Option<u32>) {
        let mut inner = self.inner.lock().unwrap();
        let state = inner
            .entry(host.to_string())
            .or_insert_with(|| self.fresh_state(host));
        state.per_minute = per_minute;
    }

    /// 现在能不能再开一条连接到 `host`。`Err` 里是建议等待时长。
    pub fn try_acquire(&self, host: &str) -> Result<(), std::time::Duration> {
        self.try_acquire_at(host, now_ms())
            .map_err(std::time::Duration::from_millis)
    }

    /// [`HostLimits::try_acquire`] 的可注入时间版本（测试用）。
    pub fn try_acquire_at(&self, host: &str, now_ms: u64) -> Result<(), u64> {
        let mut inner = self.inner.lock().unwrap();
        let state = inner
            .entry(host.to_string())
            .or_insert_with(|| self.fresh_state(host));
        state.roll_minute(now_ms);
        if now_ms < state.cooldown_until_ms {
            return Err((state.cooldown_until_ms - now_ms).max(RETRY_HINT_MS));
        }
        if let Some(per_minute) = state.per_minute {
            if state.minute_used >= per_minute {
                let next_minute = (state.minute_id + 1) * 60_000;
                return Err((next_minute - now_ms).max(RETRY_HINT_MS));
            }
        }
        if state.used >= state.budget {
            return Err(RETRY_HINT_MS);
        }
        state.used += 1;
        state.minute_used += 1;
        Ok(())
    }

    /// 一条连接结束（无论成败）。
    pub fn release(&self, host: &str) {
        let mut inner = self.inner.lock().unwrap();
        if let Some(state) = inner.get_mut(host) {
            state.used = state.used.saturating_sub(1);
        }
    }

    /// 被限流：预算减半 + 冷却。
    pub fn note_throttled(&self, host: &str, retry_after_ms: Option<u64>) {
        self.note_throttled_at(host, retry_after_ms, now_ms());
    }

    pub fn note_throttled_at(&self, host: &str, retry_after_ms: Option<u64>, now_ms: u64) {
        let mut inner = self.inner.lock().unwrap();
        let state = inner
            .entry(host.to_string())
            .or_insert_with(|| self.fresh_state(host));
        state.budget = (state.budget / 2).max(1);
        state.ok_streak = 0;
        let cooldown = retry_after_ms.unwrap_or(DEFAULT_COOLDOWN_MS).max(500);
        state.cooldown_until_ms = now_ms.saturating_add(cooldown);
    }

    /// 一次成功：攒够连续成功次数就把预算加回去。
    pub fn note_ok(&self, host: &str) {
        let mut inner = self.inner.lock().unwrap();
        let Some(state) = inner.get_mut(host) else {
            return;
        };
        state.ok_streak = state.ok_streak.saturating_add(1);
        if state.ok_streak >= RECOVER_STREAK && state.budget < state.limit {
            state.budget += 1;
            state.ok_streak = 0;
        }
    }

    /// 从响应头学到的配额：`X-RateLimit-Remaining-short` 见底就先冷却到重置时刻。
    ///
    /// 注意这里**只收紧不放松**：没被限流之前不主动按 10/分钟去掐自己——
    /// 实测 `bmclapi2` 的头是按边缘节点算的（同一客户端连续请求时
    /// `Remaining-short` 一直是 9），真按 10/分钟硬掐会把国内正常速度压死。
    pub fn observe_headers(&self, host: &str, remaining_short: Option<u32>, reset_short_s: Option<u64>) {
        self.observe_headers_at(host, remaining_short, reset_short_s, now_ms());
    }

    /// [`HostLimits::observe_headers`] 的可注入时间版本（测试用）。
    pub fn observe_headers_at(
        &self,
        host: &str,
        remaining_short: Option<u32>,
        reset_short_s: Option<u64>,
        now: u64,
    ) {
        let Some(remaining) = remaining_short else {
            return;
        };
        if remaining > 2 {
            return;
        }
        let mut inner = self.inner.lock().unwrap();
        let state = inner
            .entry(host.to_string())
            .or_insert_with(|| self.fresh_state(host));
        let cooldown = reset_short_s.unwrap_or(60).clamp(1, 300) * 1000;
        state.cooldown_until_ms = state.cooldown_until_ms.max(now.saturating_add(cooldown));
    }

    /// 某台主机现在的状态（诊断/测试/界面展示用）。
    pub fn state_of(&self, host: &str) -> Option<HostState> {
        self.inner.lock().unwrap().get(host).cloned()
    }

    /// 这台主机是否正在限流冷却期（只读，不占配额）。
    ///
    /// 选源时要用它："镜像刚被限流过就先去官方"——不然明知会被挡还硬撞。
    pub fn cooling_down(&self, host: &str) -> bool {
        self.cooling_down_at(host, now_ms())
    }

    /// [`Self::cooling_down`] 的显式时间版本（测试用）。
    pub fn cooling_down_at(&self, host: &str, now_ms: u64) -> bool {
        self.inner
            .lock()
            .unwrap()
            .get(host)
            .map(|state| state.cooldown_until_ms > now_ms)
            .unwrap_or(false)
    }

    fn fresh_state(&self, host: &str) -> HostState {
        let limit = if host.eq_ignore_ascii_case(BMCLAPI_HOST) {
            self.concurrency.min(BMCLAPI_LIMIT)
        } else {
            self.concurrency
        };
        HostState::new(limit, None)
    }
}

impl HostState {
    fn roll_minute(&mut self, now_ms: u64) {
        let minute_id = now_ms / 60_000;
        if minute_id != self.minute_id {
            self.minute_id = minute_id;
            self.minute_used = 0;
        }
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bmclapi_is_capped_at_four_connections() {
        let limits = HostLimits::for_concurrency(16);
        let host = BMCLAPI_HOST;
        for _ in 0..4 {
            assert!(limits.try_acquire_at(host, 0).is_ok());
        }
        assert!(limits.try_acquire_at(host, 0).is_err());
        limits.release(host);
        assert!(limits.try_acquire_at(host, 0).is_ok());
    }

    #[test]
    fn other_hosts_use_the_global_concurrency() {
        let limits = HostLimits::for_concurrency(8);
        for _ in 0..8 {
            assert!(limits.try_acquire_at("piston-data.mojang.com", 0).is_ok());
        }
        assert!(limits.try_acquire_at("piston-data.mojang.com", 0).is_err());
    }

    #[test]
    fn throttle_halves_budget_then_recovers_after_twenty_ok() {
        let limits = HostLimits::for_concurrency(8);
        let host = "node.example";
        limits.note_throttled_at(host, None, 0);
        assert_eq!(limits.state_of(host).unwrap().budget, 4);
        limits.note_throttled_at(host, None, 0);
        assert_eq!(limits.state_of(host).unwrap().budget, 2);
        // 冷却期内一律挡掉
        assert!(limits.try_acquire_at(host, 100).is_err());
        // 冷却过去后按 2 条预算放行
        let later = DEFAULT_COOLDOWN_MS + 1;
        assert!(limits.try_acquire_at(host, later).is_ok());
        assert!(limits.try_acquire_at(host, later).is_ok());
        assert!(limits.try_acquire_at(host, later).is_err());
        for _ in 0..RECOVER_STREAK {
            limits.note_ok(host);
        }
        assert_eq!(limits.state_of(host).unwrap().budget, 3);
    }

    #[test]
    fn retry_after_puts_host_into_cooldown_for_that_long() {
        let limits = HostLimits::for_concurrency(8);
        let host = "node.example";
        limits.note_throttled_at(host, Some(7_000), 1_000);
        let wait = limits.try_acquire_at(host, 1_000).unwrap_err();
        assert_eq!(wait, 7_000);
        assert!(limits.try_acquire_at(host, 8_000).is_ok());
    }

    #[test]
    fn per_minute_bucket_blocks_requests_beyond_the_quota() {
        let limits = HostLimits::for_concurrency(8);
        let host = "quota.example";
        limits.set_per_minute(host, Some(2));
        assert!(limits.try_acquire_at(host, 1_000).is_ok());
        assert!(limits.try_acquire_at(host, 1_000).is_ok());
        let wait = limits.try_acquire_at(host, 1_000).unwrap_err();
        assert!(wait > 0);
        // 下一分钟自动恢复
        assert!(limits.try_acquire_at(host, 61_000).is_ok());
    }

    #[test]
    fn header_observation_only_tightens_when_quota_is_almost_gone() {
        let limits = HostLimits::for_concurrency(8);
        let host = BMCLAPI_HOST;
        limits.observe_headers_at(host, Some(9), Some(60), 0);
        assert!(limits.try_acquire_at(host, 0).is_ok());

        let limits = HostLimits::for_concurrency(8);
        limits.observe_headers_at(host, Some(1), Some(30), 0);
        assert!(limits.try_acquire_at(host, 0).is_err());
        assert!(limits.try_acquire_at(host, 31_000).is_ok());
    }
}
