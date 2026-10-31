//! 账号头像的皮肤直链解析。
//!
//! 优先级：调用方显式给的 URL → Mojang 官方 profile textures → mc-heads
//! 的皮肤文件兜底。界面只负责按 `avaHead3` 的方式裁切，不猜贴图来源。

use std::time::Duration;

use base64::Engine;
use serde_json::Value;

use kaleid_api::{ApiError, ApiResult};

use crate::loaders::url_encode;
use crate::remote::RemoteClient;

const SESSION_SERVER: &str = "https://sessionserver.mojang.com/session/minecraft/profile";
const USER_LOOKUP: &str = "https://api.mojang.com/users/profiles/minecraft";
const MC_HEADS: &str = "https://mc-heads.net/skin";

pub fn resolve_skin(
    name: Option<&str>,
    uuid: Option<&str>,
    explicit: Option<&str>,
) -> ApiResult<Option<String>> {
    if let Some(url) = explicit.map(str::trim).filter(|s| !s.is_empty()) {
        return normalize_url(url).map(Some);
    }

    let id = uuid.and_then(normalize_uuid);
    let lookup_name = name.map(str::trim).filter(|s| !s.is_empty());

    // 官方 sessionserver 只需要公开 profile，不需要 access token。
    if let Some(id) = id.as_deref() {
        if let Some(url) = official_texture(id) {
            return Ok(Some(url));
        }
    } else if let Some(name) = lookup_name {
        if let Some(id) = lookup_uuid(name) {
            if let Some(url) = official_texture(&id) {
                return Ok(Some(url));
            }
            return Ok(Some(format!("{MC_HEADS}/{id}")));
        }
    }

    // UUID 对应的官方皮肤失败时，mc-heads 仍可能缓存了同名/同 UUID 的皮肤。
    if let Some(id) = id.as_deref() {
        return Ok(Some(format!("{MC_HEADS}/{id}")));
    }
    if let Some(name) = lookup_name {
        return Ok(Some(format!("{MC_HEADS}/{}", url_encode(name))));
    }
    Ok(None)
}

fn normalize_url(url: &str) -> ApiResult<String> {
    let url = url.trim();
    if !url.starts_with("https://") && !url.starts_with("http://") {
        return Err(ApiError::invalid("皮肤地址必须是 http(s) URL"));
    }
    Ok(if let Some(rest) = url.strip_prefix("http://") {
        format!("https://{rest}")
    } else {
        url.to_string()
    })
}

fn normalize_uuid(uuid: &str) -> Option<String> {
    let compact: String = uuid.chars().filter(|c| *c != '-').collect();
    if compact.len() == 32 && compact.chars().all(|c| c.is_ascii_hexdigit()) {
        Some(compact.to_ascii_lowercase())
    } else {
        None
    }
}

fn lookup_uuid(name: &str) -> Option<String> {
    let client = RemoteClient::new();
    let url = format!("{USER_LOOKUP}/{}", url_encode(name));
    let value = get_json(&client, &url)?;
    value
        .get("id")
        .and_then(|v| v.as_str())
        .and_then(normalize_uuid)
}

fn official_texture(uuid: &str) -> Option<String> {
    let client = RemoteClient::new();
    let url = format!("{SESSION_SERVER}/{uuid}");
    let value = get_json(&client, &url)?;
    let encoded = value
        .get("properties")
        .and_then(|v| v.as_array())?
        .iter()
        .find(|p| p.get("name").and_then(|n| n.as_str()) == Some("textures"))?
        .get("value")
        .and_then(|v| v.as_str())?;
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .ok()?;
    let textures: Value = serde_json::from_slice(&decoded).ok()?;
    let url = textures
        .get("textures")
        .and_then(|v| v.get("SKIN"))
        .and_then(|v| v.get("url"))
        .and_then(|v| v.as_str())?;
    normalize_url(url).ok()
}

fn get_json(client: &RemoteClient, url: &str) -> Option<Value> {
    let response = client
        .agent()
        .get(url)
        .timeout(Duration::from_secs(6))
        .call()
        .ok()?;
    if response.status() != 200 {
        return None;
    }
    response.into_json::<Value>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_url_wins_and_http_is_upgraded() {
        let url = resolve_skin(
            Some("ignored"),
            Some("ignored"),
            Some("http://textures.minecraft.net/x"),
        )
        .unwrap()
        .unwrap();
        assert_eq!(url, "https://textures.minecraft.net/x");
        assert!(resolve_skin(None, None, Some("file:///tmp/x")).is_err());
    }

    #[test]
    fn uuid_normalization_rejects_short_values() {
        assert_eq!(
            normalize_uuid("069A79F4-44E9-4726-A5BE-FCA90E38AFFC").as_deref(),
            Some("069a79f444e94726a5befca90e38affc")
        );
        assert_eq!(normalize_uuid("not-a-uuid"), None);
    }
}
