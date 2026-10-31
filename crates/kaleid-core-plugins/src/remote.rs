//! Mojang 版本清单、版本分类与远程 JSON 抓取。

use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

use kaleid_api::{ApiError, ApiResult, RemoteVersion, RemoteVersions};

pub const OFFICIAL_MANIFEST: &str =
    "https://piston-meta.mojang.com/mc/game/version_manifest_v2.json";
pub const MIRROR_MANIFEST: &str =
    "https://bmclapi2.bangbang93.com/mc/game/version_manifest_v2.json";

pub struct RemoteClient {
    agent: ureq::Agent,
    cache: Mutex<Option<(Instant, RemoteVersions)>>,
}

impl RemoteClient {
    pub fn new() -> Self {
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(15))
            .timeout_read(Duration::from_secs(45))
            .user_agent("KaleidLauncher/0.1")
            .build();
        RemoteClient {
            agent,
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
        if !force {
            if let Some((at, cached)) = self.cache.lock().unwrap().as_ref() {
                if at.elapsed() < Duration::from_secs(300) {
                    return Ok(cached.clone());
                }
            }
        }
        let value = match self.get_json(OFFICIAL_MANIFEST) {
            Ok(v) => v,
            Err(_) => self.get_json(MIRROR_MANIFEST)?,
        };
        let versions = parse_manifest(&value)?;
        *self.cache.lock().unwrap() = Some((Instant::now(), versions.clone()));
        Ok(versions)
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
