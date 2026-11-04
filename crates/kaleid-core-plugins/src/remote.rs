//! Mojang 版本清单、版本分类与远程 JSON 抓取。

use std::io::Read;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

use kaleid_api::{ApiError, ApiResult, RemoteVersion, RemoteVersions};

pub const OFFICIAL_MANIFEST: &str =
    "https://piston-meta.mojang.com/mc/game/version_manifest_v2.json";
pub const MIRROR_MANIFEST: &str = crate::mirrors::BMCLAPI_MANIFEST_URL;

pub struct RemoteClient {
    agent: ureq::Agent,
    /// 元数据换源用的短超时客户端：连不上就换下一个地址，不把安装卡在官方源。
    meta_agent: ureq::Agent,
    cache: Mutex<Option<(Instant, String, RemoteVersions)>>,
}

impl RemoteClient {
    pub fn new() -> Self {
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(15))
            .timeout_read(Duration::from_secs(45))
            .max_idle_connections_per_host(16)
            .user_agent("KaleidLauncher/0.1")
            .build();
        let meta_agent = ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(4))
            .timeout_read(Duration::from_secs(30))
            .max_idle_connections_per_host(16)
            .user_agent("KaleidLauncher/0.1")
            .build();
        RemoteClient {
            agent,
            meta_agent,
            cache: Mutex::new(None),
        }
    }

    pub fn agent(&self) -> &ureq::Agent {
        &self.agent
    }

    pub fn get_json(&self, url: &str) -> ApiResult<Value> {
        self.with_retry(|| {
            let response = self
                .agent
                .get(url)
                .call()
                .map_err(|e| ApiError::Network(format!("{url}: {e}")))?;
            response
                .into_json::<Value>()
                .map_err(|e| ApiError::Network(format!("{url}: {e}")))
        })
    }

    pub fn get_text(&self, url: &str) -> ApiResult<String> {
        self.with_retry(|| {
            let response = self
                .agent
                .get(url)
                .call()
                .map_err(|e| ApiError::Network(format!("{url}: {e}")))?;
            response
                .into_string()
                .map_err(|e| ApiError::Network(format!("{url}: {e}")))
        })
    }

    /// 元数据请求的重试：一次网络抖动不该让整个安装停摆（例如装 Fabric 时先要读
    /// Modrinth 的 Fabric API 版本）。**只对网络错误重试**——格式错、404 再请求一次
    /// 也是白费，还会把真正的错误信息盖掉。
    fn with_retry<T>(&self, request: impl Fn() -> ApiResult<T>) -> ApiResult<T> {
        let first = match request() {
            Ok(value) => return Ok(value),
            Err(err) => err,
        };
        if !matches!(first, ApiError::Network(_)) {
            return Err(first);
        }
        std::thread::sleep(Duration::from_millis(400));
        request()
    }

    pub fn versions(&self, force: bool) -> ApiResult<RemoteVersions> {
        self.versions_with_policy(force, kaleid_api::DOWNLOAD_SOURCE_OFFICIAL_FIRST)
    }

    /// 按下载源策略拉版本清单。缓存键带策略，避免「仅镜像」命中之前的官方结果。
    pub fn versions_with_policy(&self, force: bool, policy: &str) -> ApiResult<RemoteVersions> {
        if !force {
            if let Some((at, cached_policy, cached)) = self.cache.lock().unwrap().as_ref() {
                if cached_policy == policy && at.elapsed() < Duration::from_secs(300) {
                    return Ok(cached.clone());
                }
            }
        }
        let urls = meta_url_pair(OFFICIAL_MANIFEST, MIRROR_MANIFEST, policy);
        let value = self.fetch_json_ordered(&urls)?;
        let versions = parse_manifest(&value)?;
        *self.cache.lock().unwrap() = Some((Instant::now(), policy.to_string(), versions.clone()));
        Ok(versions)
    }

    /// 按顺序试地址。每个候选连接 4 秒、读取 30 秒，失败立刻换下一个，不再对死地址多睡一次。
    pub fn fetch_json_ordered(&self, urls: &[String]) -> ApiResult<Value> {
        self.fetch_ordered(urls, |response| {
            response
                .into_json::<Value>()
                .map_err(|e| ApiError::Network(e.to_string()))
        })
    }

    pub fn fetch_text_ordered(&self, urls: &[String]) -> ApiResult<String> {
        self.fetch_ordered(urls, |response| {
            response
                .into_string()
                .map_err(|e| ApiError::Network(e.to_string()))
        })
    }

    fn fetch_ordered<T>(
        &self,
        urls: &[String],
        read: impl Fn(ureq::Response) -> ApiResult<T>,
    ) -> ApiResult<T> {
        if urls.is_empty() {
            return Err(ApiError::Network("没有可用的元数据地址".into()));
        }
        let mut last = ApiError::Network("没有可用的元数据地址".into());
        for url in urls {
            match self.meta_agent.get(url).call() {
                Ok(response) => match read(response) {
                    Ok(value) => return Ok(value),
                    Err(error) => last = error,
                },
                Err(error) => last = ApiError::Network(format!("{url}: {error}")),
            }
        }
        Err(last)
    }

    /// 抓一个"必须真的是 JSON"的远程文件（资源索引、版本 JSON 之类）。
    ///
    /// 为什么不能直接 `into_json`：镜像站被限流或上游出错时，常常回 **200 + 一段
    /// HTML 错误页**。这时候 serde 只会吐一句 `expected value at line 1 column 1`，
    /// 用户看到的"JSON 解析失败"其实跟 JSON 一点关系都没有。这里做四件事：
    ///
    /// 1. 非 2xx 直接记一笔，换下一个地址；
    /// 2. `Content-Type` 明显不是 JSON 就拒收，并把正文开头摘出来（HTML 的话能
    ///    一眼看出是 429 / 403 还是 502）；
    /// 3. `Content-Length` 或实际读取超过上限就立即失败，防止镜像把整个 jar
    ///    当 JSON 灌过来；
    /// 4. **内容错误不重试同一个地址**——坏地址撞两次只是让用户多等一轮；
    ///    只有连接失败/读超时这种网络抖动才在同一地址上再试一次。
    pub fn fetch_verified_json(&self, urls: &[String], what: &str) -> ApiResult<Value> {
        self.fetch_verified_json_capped(urls, what, VERIFY_MAX_BYTES)
    }

    /// 同 [`Self::fetch_verified_json`]，但允许调用方按已知文件大小放宽上限
    /// （资源索引会按 `assetIndex.size` 放宽，避免未来索引真超过 16 MiB 被误杀）。
    pub fn fetch_verified_json_capped(
        &self,
        urls: &[String],
        what: &str,
        max_bytes: usize,
    ) -> ApiResult<Value> {
        self.fetch_validated_bytes(urls, what, max_bytes, |meta| {
            serde_json::from_slice::<Value>(&meta.bytes).map_err(|error| {
                format!(
                    "返回的内容不是合法的 JSON（{error}）：{}",
                    snippet(&String::from_utf8_lossy(&meta.bytes))
                )
            })
        }).map(|(_, value)| value)
    }

    /// 抓取并校验一段元数据，返回原始字节。
    ///
    /// 调用方拿到字节后可以自己落盘再解析——资源索引只下载一次，不必
    /// "下载项下一次 + 解析时再一次"。
    pub fn fetch_verified_bytes(
        &self,
        urls: &[String],
        what: &str,
        max_bytes: usize,
    ) -> ApiResult<VerifiedBytes> {
        self.fetch_validated_bytes(urls, what, max_bytes, |_| Ok(())).map(|(meta, ())| meta)
    }

    /// 每个候选只取一次正文；调用方的内容校验失败时直接换源，不消耗传输重试预算。
    pub(crate) fn fetch_validated_bytes<T>(
        &self,
        urls: &[String],
        what: &str,
        max_bytes: usize,
        validate: impl Fn(&VerifiedBytes) -> Result<T, String>,
    ) -> ApiResult<(VerifiedBytes, T)> {
        if urls.is_empty() {
            return Err(ApiError::Network(format!("{what}：没有可用的地址")));
        }
        let mut failures: Vec<String> = Vec::new();
        for url in urls {
            let host = host_of(url);
            match self.meta_attempt(url, what, &host, max_bytes) {
                Ok(bytes) => {
                    let meta = VerifiedBytes { bytes, url: url.clone() };
                    match validate(&meta) {
                        Ok(value) => return Ok((meta, value)),
                        Err(reason) => failures.push(format!("{url} → {reason}")),
                    }
                }
                Err(reason) => failures.push(format!("{url} → {reason}")),
            }
        }
        Err(ApiError::Network(format!(
            "{what}下载失败：{}",
            failures.join("；")
        )))
    }

    /// 单个地址：网络错误最多试两次，内容错误立即返回。
    fn meta_attempt(
        &self,
        url: &str,
        what: &str,
        host: &str,
        max_bytes: usize,
    ) -> Result<Vec<u8>, String> {
        let mut last = format!("{host}：没有尝试请求");
        for attempt in 0..META_ATTEMPTS {
            match self
                .meta_agent
                .get(url)
                .set("Accept", "application/json")
                .call()
            {
                Ok(response) => return verified_body(response, what, host, max_bytes),
                Err(error) => {
                    let transport = matches!(error, ureq::Error::Transport(_));
                    last = format!("{host}：{error}");
                    // HTTP 4xx/5xx 再打一次也是同样的答复；只有传输层错误值得重试。
                    if !transport {
                        break;
                    }
                    if attempt + 1 < META_ATTEMPTS {
                        std::thread::sleep(Duration::from_millis(META_PAUSE_MS));
                    }
                }
            }
        }
        Err(last)
    }


    pub fn version_json_url(&self, version_id: &str) -> ApiResult<String> {
        let versions = self.versions(false)?;
        versions
            .versions
            .iter()
            .find(|v| v.id == version_id)
            .map(|v| v.url.clone())
            .filter(|url| !url.is_empty())
            .ok_or_else(|| ApiError::not_found(format!("版本清单中没有 {version_id}")))
    }
}

/// 元数据"必须是 JSON"的抓取 read 多少字节、网络错误在同一地址重试几次。
pub const VERIFY_MAX_BYTES: usize = 16 * 1024 * 1024;
const META_ATTEMPTS: usize = 2;
const META_PAUSE_MS: u64 = 400;
/// 报错时摘多少字节的正文——够看清是 429 还是 502 就行。
const SNIPPET_BYTES: usize = 120;

/// 一次校验过的元数据：原始字节 + 实际命中的地址。
#[derive(Debug, Clone)]
pub struct VerifiedBytes {
    pub bytes: Vec<u8>,
    pub url: String,
}

fn host_of(url: &str) -> String {
    crate::urlcache::host_of(url)
}

/// 把一段正文压成一行，放进中文错误信息里。
fn snippet(text: &str) -> String {
    let head: String = text.trim_start().chars().take(SNIPPET_BYTES).collect();
    let mut clean: String = head
        .chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .collect();
    if text.trim_start().chars().count() > SNIPPET_BYTES {
        clean.push('…');
    }
    clean
}

/// 校验并读出正文：拦掉"200 + HTML 错误页"和体积离谱的响应。
fn verified_body(
    response: ureq::Response,
    _what: &str,
    host: &str,
    max_bytes: usize,
) -> Result<Vec<u8>, String> {
    let content_type = response.header("content-type").unwrap_or("").to_string();
    // 先看声明大小：镜像把 40 MB 的 client.jar 当索引时，不必读满 16 MiB 才发现。
    if let Some(length) = response
        .header("content-length")
        .and_then(|value| value.trim().parse::<usize>().ok())
    {
        if length > max_bytes {
            return Err(format!(
                "{host} 返回的内容过大（{length} 字节，超过 {} 字节上限）；这个地址不是元数据",
                max_bytes
            ));
        }
    }
    let mut reader = response.into_reader();
    let mut buffer: Vec<u8> = Vec::with_capacity(64 * 1024);
    let mut chunk = [0u8; 64 * 1024];
    loop {
        let read = reader
            .read(&mut chunk)
            .map_err(|e| format!("{host} 读取中断：{e}"))?;
        if read == 0 {
            break;
        }
        if buffer.len() + read > max_bytes {
            return Err(format!(
                "{host} 返回的内容过大（已读超过 {} 字节上限）；这个地址不是元数据",
                max_bytes
            ));
        }
        buffer.extend_from_slice(&chunk[..read]);
    }
    let text = String::from_utf8_lossy(&buffer);
    let trimmed = text.trim_start_matches('\u{feff}').trim_start();
    // 明显是 HTML / XML：这类响应一定是错误页，不需要再让 serde 猜。
    let looks_markup = trimmed.starts_with('<');
    let says_json = content_type.to_ascii_lowercase().contains("json");
    if looks_markup || (!says_json && content_type.to_ascii_lowercase().contains("html")) {
        return Err(format!(
            "返回的不是 JSON（Content-Type: {}）：{}",
            if content_type.is_empty() {
                "未知"
            } else {
                &content_type
            },
            snippet(trimmed)
        ));
    }
    Ok(buffer)
}

/// 同 [`meta_url_pair`]，但给"官方主机在国内常年超时"的元数据用（Forge / NeoForge
/// 的 Maven）：除非玩家选了「仅官方」，否则镜像排第一——和 `DownloadItem::prefer_mirror`
/// 一个规则，免得每次安装都先白等一次官方超时。
pub fn meta_url_pair_prefer_mirror(official: &str, mirror: &str, policy: &str) -> Vec<String> {
    let policy = policy.trim_start_matches("mod-");
    match policy {
        kaleid_api::DOWNLOAD_SOURCE_OFFICIAL_ONLY => vec![official.to_string()],
        kaleid_api::DOWNLOAD_SOURCE_MIRROR_ONLY => vec![mirror.to_string()],
        _ => vec![mirror.to_string(), official.to_string()],
    }
}

/// 官方 / 镜像两个元数据地址按下载源策略排序。`mirror-only` 不包含官方地址。
pub fn meta_url_pair(official: &str, mirror: &str, policy: &str) -> Vec<String> {
    let mirror_first = policy == kaleid_api::DOWNLOAD_SOURCE_MIRROR_FIRST
        || policy == kaleid_api::DOWNLOAD_SOURCE_MIRROR_ONLY
        || policy == kaleid_api::DOWNLOAD_SOURCE_AUTO;
    let mirror_only = policy == kaleid_api::DOWNLOAD_SOURCE_MIRROR_ONLY;
    let official_only = policy == kaleid_api::DOWNLOAD_SOURCE_OFFICIAL_ONLY;
    let mut urls = Vec::new();
    if mirror_first {
        urls.push(mirror.to_string());
        if !mirror_only {
            urls.push(official.to_string());
        }
    } else {
        urls.push(official.to_string());
        if !official_only {
            urls.push(mirror.to_string());
        }
    }
    urls
}

pub fn parse_manifest(value: &Value) -> ApiResult<RemoteVersions> {
    let latest = value.get("latest").cloned().unwrap_or_else(|| json!({}));
    let versions = value
        .get("versions")
        .and_then(|v| v.as_array())
        .ok_or_else(|| ApiError::Format("版本清单缺少 versions 数组".into()))?;
    let mut out = Vec::with_capacity(versions.len());
    for item in versions {
        let id = item
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ApiError::Format("版本清单条目缺少 id".into()))?;
        let kind = item
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("snapshot");
        let release_time = item
            .get("releaseTime")
            .or_else(|| item.get("time"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let url = item
            .get("url")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let (category, year, intro) = classify(id, kind);
        out.push(RemoteVersion {
            id: id.to_string(),
            kind: kind.to_string(),
            release_time: release_time.to_string(),
            url,
            category: category.to_string(),
            april_fools_year: year,
            april_fools_intro: intro.map(|s| s.to_string()),
        });
    }
    Ok(RemoteVersions {
        latest_release: latest
            .get("release")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        latest_snapshot: latest
            .get("snapshot")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        versions: out,
        fetched_at: now_secs(),
    })
}

pub fn classify(id: &str, kind: &str) -> (&'static str, Option<u32>, Option<&'static str>) {
    if let Some((year, intro)) = april_fools(id) {
        return ("april_fools", Some(year), Some(intro));
    }
    match kind {
        "release" => ("release", None, None),
        "old_beta" | "old_alpha" => ("ancient", None, None),
        _ => ("snapshot", None, None),
    }
}

/// 精确 ID 表。不能按日期或 `w14` 猜测：20w14a 等是普通快照。
pub fn april_fools(id: &str) -> Option<(u32, &'static str)> {
    match id {
        "15w14a" => Some((2015, "爱与拥抱更新")),
        "1.RV-Pre1" => Some((2016, "时尚更新")),
        "3D Shareware v1.34" => Some((2019, "3D 共享软件")),
        "20w14infinite" => Some((2020, "终极内容更新")),
        "22w13oneblockatatime" => Some((2022, "一次一个方块更新")),
        "23w13a_or_b" => Some((2023, "投票更新")),
        "24w14potato" => Some((2024, "毒马铃薯更新")),
        "25w14craftmine" => Some((2025, "Craftmine 更新")),
        "26w14a" => Some((2026, "为什么需要物品栏？让方块们跟着你走吧！")),
        _ => None,
    }
}

pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefer_mirror_pair_respects_only_policies() {
        let o = "https://maven.neoforged.net/x.sha1";
        let m = "https://bmclapi2.bangbang93.com/maven/x.sha1";
        for policy in ["auto", "official-first", "mirror-first"] {
            assert_eq!(meta_url_pair_prefer_mirror(o, m, policy), vec![m, o], "{policy}");
        }
        assert_eq!(meta_url_pair_prefer_mirror(o, m, "official-only"), vec![o]);
        assert_eq!(meta_url_pair_prefer_mirror(o, m, "mirror-only"), vec![m]);
    }

    #[test]
    fn auto_metadata_starts_on_the_mirror_side() {
        assert_eq!(meta_url_pair("o", "m", "auto"), vec!["m", "o"]);
        assert_eq!(meta_url_pair("o", "m", "official-first"), vec!["o", "m"]);
    }

    #[test]
    fn mirror_only_metadata_urls_skip_mojang() {
        let urls = meta_url_pair(
            "https://piston-meta.mojang.com/mc/game/version_manifest_v2.json",
            "https://bmclapi2.bangbang93.com/mc/game/version_manifest_v2.json",
            kaleid_api::DOWNLOAD_SOURCE_MIRROR_ONLY,
        );
        assert_eq!(urls.len(), 1);
        assert!(urls[0].contains("bmclapi2.bangbang93.com"));
        assert!(urls.iter().all(|url| {
            !url.contains("piston-meta.mojang.com")
                && !url.contains("launchermeta.mojang.com")
                && !url.contains("libraries.minecraft.net")
        }));
    }

    #[test]
    fn april_fools_ids_are_exact() {
        assert_eq!(april_fools("26w14a").unwrap().0, 2026);
        assert_eq!(april_fools("20w14infinite").unwrap().0, 2020);
        assert!(april_fools("20w14a").is_none());
        assert!(april_fools("22w13a").is_none());
        assert!(april_fools("23w13a").is_none());
        assert!(april_fools("24w14a").is_none());
    }

    #[test]
    fn categories_cover_release_snapshot_ancient_and_joke() {
        assert_eq!(classify("26.1.2", "release").0, "release");
        assert_eq!(classify("26w14a", "snapshot").0, "april_fools");
        assert_eq!(classify("a1.2.6", "old_alpha").0, "ancient");
        assert_eq!(classify("26w13a", "snapshot").0, "snapshot");
    }

    #[test]
    fn parse_manifest_keeps_original_type_and_release_time() {
        let v = json!({
            "latest": {"release": "26.1.2", "snapshot": "26w14a"},
            "versions": [
                {"id": "26.1.2", "type": "release", "releaseTime": "2026-04-09T10:00:00+00:00"},
                {"id": "26w14a", "type": "snapshot", "releaseTime": "2026-04-01T20:13:30+00:00"}
            ]
        });
        let parsed = parse_manifest(&v).unwrap();
        assert_eq!(parsed.latest_release.as_deref(), Some("26.1.2"));
        assert_eq!(parsed.versions[1].category, "april_fools");
        assert_eq!(parsed.versions[1].april_fools_year, Some(2026));
    }
}
