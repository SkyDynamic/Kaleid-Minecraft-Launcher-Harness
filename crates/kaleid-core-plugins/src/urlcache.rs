//! 重定向解析缓存：同一个地址只解析一次。
//!
//! 现实里 BMCLAPI 是**重定向服务**而不是文件源（2026-11 实测）：
//!
//! | 请求 | 链 |
//! |---|---|
//! | `/version/<id>/client` | 302 → `/v1/objects/<sha>/client.jar` → 302 → `piston-data.mojang.com/…` |
//! | `/maven/…jar` | 302 → `mirrors.cernet.edu.cn/bmclapi/…` → 302 → `chinanet.mirrors.ustc.edu.cn/…` |
//! | `/assets/<p>/<h>` | 302 → `<节点主机>/download/…` |
//!
//! 每一跳都要问一次 `bmclapi2`，而它带着 `X-RateLimit-Limit-long: 1000 / 3600s` 的
//! 配额。一次原版安装是 4000+ 个文件，大文件按 Range 分段时同一个地址还会被重复问
//! 8 次——这就是「只有几 KB/s 还触发 429」的来源之一。
//!
//! 所以这里的职责很窄：**同一个地址只解析一次**，结果记在内存 + 磁盘（跨进程复用），
//! 真正的分段/重试请求直接打到最终地址。解析失败绝不报错——拿不到就按原地址下载，
//! 让下载器自己的重试与换源逻辑说话。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// 解析结果能用多久。上游换节点不影响"这个地址大概会去哪"，6 小时覆盖一次安装绰绰有余。
const TTL_SECS: u64 = 6 * 3600;
/// 最多跟几跳。真实链路是 2 跳，5 跳足够；超了就按当前地址返回。
const MAX_HOPS: u8 = 5;
/// 内存/磁盘里最多留多少条，超了按时间淘汰最旧的四分之一。
const MAX_ENTRIES: usize = 4096;
/// 写盘节流：一次安装会新增几千条，逐条写盘没有必要。
const FLUSH_INTERVAL: Duration = Duration::from_secs(2);
const CACHE_FILE: &str = "urls.json";

/// 一次解析的结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Resolved {
    /// 跟完重定向后的地址。
    pub final_url: String,
    /// 跳了几次；`0` 表示这个地址本身就是终点。
    pub hops: u8,
    /// 终点主机（含端口，小写）。限速与选源都按它记账。
    pub host: String,
    /// 解析时间（Unix 秒），用于 TTL。
    pub at: u64,
}

impl Resolved {
    fn direct(url: &str) -> Self {
        Resolved {
            final_url: url.to_string(),
            hops: 0,
            host: host_of(url),
            at: now_secs(),
        }
    }
}

#[derive(Default, Serialize, Deserialize)]
struct CacheFile {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    entries: HashMap<String, Resolved>,
}

struct Inner {
    entries: HashMap<String, Resolved>,
    dirty: bool,
    last_flush: Instant,
}

/// 重定向解析缓存。`open` 失败不报错：缓存只是加速，不是正确性依赖。
pub struct UrlCache {
    path: Option<PathBuf>,
    inner: Mutex<Inner>,
}

impl UrlCache {
    /// `dir` 一般是 `<数据目录>/download-cache`。
    pub fn open(dir: &Path) -> Self {
        let path = dir.join(CACHE_FILE);
        let entries = read_entries(&path);
        UrlCache {
            path: Some(path),
            inner: Mutex::new(Inner {
                entries,
                dirty: false,
                last_flush: Instant::now(),
            }),
        }
    }

    /// 只留在内存里的实现（测试与"没有数据目录"的场景）。
    pub fn in_memory() -> Self {
        UrlCache {
            path: None,
            inner: Mutex::new(Inner {
                entries: HashMap::new(),
                dirty: false,
                last_flush: Instant::now(),
            }),
        }
    }

    /// 查缓存（含 TTL 判定）。
    pub fn get(&self, url: &str) -> Option<Resolved> {
        let now = now_secs();
        let inner = self.inner.lock().unwrap();
        let entry = inner.entries.get(url)?;
        if now.saturating_sub(entry.at) > TTL_SECS {
            return None;
        }
        Some(entry.clone())
    }

    /// 记一条（并择机落盘）。
    pub fn put(&self, url: &str, resolved: Resolved) {
        {
            let mut inner = self.inner.lock().unwrap();
            inner.entries.insert(url.to_string(), resolved);
            inner.dirty = true;
            if inner.entries.len() > MAX_ENTRIES {
                evict_oldest(&mut inner.entries);
            }
            if inner.last_flush.elapsed() < FLUSH_INTERVAL {
                return;
            }
            inner.last_flush = Instant::now();
        }
        self.flush();
    }

    /// 立刻落盘。`Drop` 里也会调一次，所以进程正常退出不会丢缓存。
    pub fn flush(&self) {
        let Some(path) = self.path.as_ref() else {
            return;
        };
        let payload = {
            let mut inner = self.inner.lock().unwrap();
            if !inner.dirty {
                return;
            }
            inner.dirty = false;
            CacheFile {
                version: 1,
                entries: inner.entries.clone(),
            }
        };
        let Ok(bytes) = serde_json::to_vec(&payload) else {
            return;
        };
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        // 先写临时文件再改名：半个 JSON 会让下次启动的缓存整块作废。
        let tmp = path.with_extension("json.tmp");
        if std::fs::write(&tmp, &bytes).is_ok() {
            let _ = std::fs::rename(&tmp, path);
        }
    }

    /// 解析一个地址：先查缓存，没有就手动跟一遍重定向。
    pub fn resolve(&self, url: &str) -> Resolved {
        self.resolve_with(live_fetch, url)
    }

    /// 同 [`UrlCache::resolve`]，但取头的方式由调用方给（测试注入假响应）。
    pub fn resolve_with<F>(&self, fetch: F, url: &str) -> Resolved
    where
        F: Fn(&str) -> Option<(u16, Option<String>)>,
    {
        let url = url.trim();
        if let Some(hit) = self.get(url) {
            return hit;
        }
        let resolved = walk_redirects(&fetch, url);
        self.put(url, resolved.clone());
        resolved
    }
}

impl Drop for UrlCache {
    fn drop(&mut self) {
        // `flush` 会再取一次锁；先把自己这边的引用放掉。
        let dirty = self
            .inner
            .lock()
            .map(|inner| inner.dirty)
            .unwrap_or(false);
        if dirty {
            self.flush();
        }
    }
}

/// 手动跟重定向。返回**最后一个非 3xx 的地址**（或原地址）。
fn walk_redirects<F>(fetch: &F, url: &str) -> Resolved
where
    F: Fn(&str) -> Option<(u16, Option<String>)>,
{
    if !is_http(url) {
        return Resolved::direct(url);
    }
    let start = url.to_string();
    let mut current = start.clone();
    let mut hops = 0u8;
    while hops < MAX_HOPS {
        let Some((status, location)) = fetch(&current) else {
            break;
        };
        if !is_redirect(status) {
            break;
        }
        let Some(location) = location.map(|l| l.trim().to_string()).filter(|l| !l.is_empty()) else {
            break;
        };
        let next = join_location(&current, &location);
        // 跳到非 http(s)（比如 `data:`）或没变化：不再往下走，按当前地址用。
        if !is_http(&next) || next == current {
            break;
        }
        hops += 1;
        current = next;
    }
    if hops == 0 {
        return Resolved::direct(&start);
    }
    Resolved {
        final_url: current.clone(),
        hops,
        host: host_of(&current),
        at: now_secs(),
    }
}

/// 把 `Location` 拼成绝对地址。支持绝对地址、`//host/path`、`/abs/path`、相对路径。
pub fn join_location(base: &str, location: &str) -> String {
    if is_http(location) {
        return location.to_string();
    }
    let (scheme, rest) = match base.split_once("://") {
        Some((scheme, rest)) => (scheme, rest),
        None => return location.to_string(),
    };
    // 去掉 query/fragment 后再取路径
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..authority_end];
    let path_and_more = &rest[authority_end..];
    let path = path_and_more
        .split(['?', '#'])
        .next()
        .unwrap_or(path_and_more);
    if let Some(stripped) = location.strip_prefix("//") {
        return format!("{scheme}://{stripped}");
    }
    if location.starts_with('/') {
        return format!("{scheme}://{authority}{location}");
    }
    // 相对路径：拿来替换最后一段
    let dir = match path.rfind('/') {
        Some(index) => &path[..=index],
        None => "/",
    };
    format!("{scheme}://{authority}{dir}{location}")
}

/// 主机标识（含端口、小写、去掉 userinfo）。限速与选源都按它记账。
pub fn host_of(url: &str) -> String {
    let rest = match url.split_once("://") {
        Some((_, rest)) => rest,
        None => url,
    };
    let host = rest
        .split(['/', '?', '#'])
        .next()
        .unwrap_or(rest);
    let host = match host.rsplit_once('@') {
        Some((_, tail)) => tail,
        None => host,
    };
    host.trim().to_ascii_lowercase()
}

fn is_redirect(status: u16) -> bool {
    matches!(status, 301 | 302 | 303 | 307 | 308)
}

fn is_http(url: &str) -> bool {
    let lower = url.trim_start().to_ascii_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://")
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn read_entries(path: &Path) -> HashMap<String, Resolved> {
    let Ok(bytes) = std::fs::read(path) else {
        return HashMap::new();
    };
    let Ok(file) = serde_json::from_slice::<CacheFile>(&bytes) else {
        return HashMap::new();
    };
    let now = now_secs();
    file.entries
        .into_iter()
        .filter(|(_, entry)| now.saturating_sub(entry.at) <= TTL_SECS)
        .collect()
}

fn evict_oldest(entries: &mut HashMap<String, Resolved>) {
    let mut ages: Vec<(u64, String)> = entries
        .iter()
        .map(|(url, entry)| (entry.at, url.clone()))
        .collect();
    ages.sort_by_key(|(at, _)| *at);
    let drop_count = (MAX_ENTRIES / 4).max(1);
    for (_, url) in ages.into_iter().take(drop_count) {
        entries.remove(&url);
    }
}

/// 真实取头：先 `HEAD`，服务器不认就 `GET` 但只读头、立刻断开。
fn live_fetch(url: &str) -> Option<(u16, Option<String>)> {
    let agent = resolve_agent();
    match agent.head(url).call() {
        Ok(response) => Some((response.status(), location_of(&response))),
        Err(ureq::Error::Status(code, response)) => Some((code, location_of(&response))),
        Err(_) => match agent.get(url).call() {
            Ok(response) => {
                let status = response.status();
                let location = location_of(&response);
                // 不要 body：这里只关心跳到哪儿。
                drop(response.into_reader());
                Some((status, location))
            }
            Err(ureq::Error::Status(code, response)) => Some((code, location_of(&response))),
            Err(_) => None,
        },
    }
}

fn location_of(response: &ureq::Response) -> Option<String> {
    response.header("location").map(str::to_string)
}

fn resolve_agent() -> &'static ureq::Agent {
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    AGENT.get_or_init(|| {
        ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(6))
            .timeout_read(Duration::from_secs(10))
            // 关键：自己跟跳，才能记住每一跳的终点。
            .redirects(0)
            .user_agent(crate::USER_AGENT)
            .build()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "kaleid-urlcache-{tag}-{}-{}",
            std::process::id(),
            now_secs()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn resolve_follows_302_chain_and_caches_final_url() {
        let calls = Cell::new(0usize);
        let fetch = |url: &str| {
            calls.set(calls.get() + 1);
            match url {
                "https://bmclapi2.bangbang93.com/version/1.21.4/client" => {
                    Some((302, Some("/v1/objects/a7e5/client.jar".to_string())))
                }
                "https://bmclapi2.bangbang93.com/v1/objects/a7e5/client.jar" => Some((
                    302,
                    Some("https://piston-data.mojang.com/v1/objects/a7e5/client.jar".to_string()),
                )),
                "https://piston-data.mojang.com/v1/objects/a7e5/client.jar" => Some((206, None)),
                _ => None,
            }
        };
        let cache = UrlCache::in_memory();
        let first = cache.resolve_with(fetch, "https://bmclapi2.bangbang93.com/version/1.21.4/client");
        assert_eq!(first.hops, 2);
        assert_eq!(
            first.final_url,
            "https://piston-data.mojang.com/v1/objects/a7e5/client.jar"
        );
        assert_eq!(first.host, "piston-data.mojang.com");
        assert_eq!(calls.get(), 3);

        // 第二次必须命中缓存：一次反查都不该发生（这正是省配额的地方）。
        let again = cache.resolve_with(fetch, "https://bmclapi2.bangbang93.com/version/1.21.4/client");
        assert_eq!(calls.get(), 3);
        assert_eq!(again, first);
    }

    #[test]
    fn resolve_gives_up_after_five_hops_without_error() {
        let calls = Cell::new(0usize);
        let fetch = |url: &str| {
            calls.set(calls.get() + 1);
            Some((302, Some(format!("{url}/x"))))
        };
        let cache = UrlCache::in_memory();
        let resolved = cache.resolve_with(fetch, "https://a.example/start");
        assert_eq!(resolved.hops, MAX_HOPS);
        // 跳满 5 次就停手：不再多问一次第 6 跳。
        assert_eq!(calls.get(), MAX_HOPS as usize);
        assert!(resolved.final_url.starts_with("https://a.example/start"));
    }

    #[test]
    fn unreachable_host_falls_back_to_the_original_url() {
        let cache = UrlCache::in_memory();
        let resolved = cache.resolve_with(|_| None, "https://b.example/path/file.jar");
        assert_eq!(resolved.hops, 0);
        assert_eq!(resolved.final_url, "https://b.example/path/file.jar");
        assert_eq!(resolved.host, "b.example");
    }

    #[test]
    fn relative_location_is_joined_against_current_url() {
        assert_eq!(join_location("https://h/a/b/c", "/x/y"), "https://h/x/y");
        assert_eq!(join_location("https://h/a/b/c", "d"), "https://h/a/b/d");
        assert_eq!(join_location("https://h/a/b/c", "//o:1/z"), "https://o:1/z");
        assert_eq!(join_location("https://h", "x"), "https://h/x");
        assert_eq!(
            join_location("https://h/a?q=1", "https://other/z"),
            "https://other/z"
        );
    }

    #[test]
    fn cache_persists_to_disk_and_skips_expired_entries() {
        let dir = scratch("disk");
        let fresh = Resolved {
            final_url: "https://piston-data.mojang.com/x".into(),
            hops: 1,
            host: "piston-data.mojang.com".into(),
            at: now_secs(),
        };
        let stale = Resolved {
            final_url: "https://old.example/x".into(),
            hops: 1,
            host: "old.example".into(),
            at: now_secs().saturating_sub(TTL_SECS + 60),
        };
        {
            let cache = UrlCache::open(&dir);
            cache.put("https://keep.example/a", fresh.clone());
            cache.put("https://stale.example/a", stale);
            cache.flush();
        }
        let reopened = UrlCache::open(&dir);
        assert_eq!(reopened.get("https://keep.example/a"), Some(fresh));
        assert_eq!(reopened.get("https://stale.example/a"), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn host_of_skips_scheme_and_path_but_keeps_port() {
        assert_eq!(
            host_of("https://bmclapi2.bangbang93.com/version/1/json"),
            "bmclapi2.bangbang93.com"
        );
        assert_eq!(
            host_of("http://127.0.0.1:38121/files/a.jar"),
            "127.0.0.1:38121"
        );
        assert_eq!(host_of("https://User:pw@h.example/x"), "h.example");
    }
}
