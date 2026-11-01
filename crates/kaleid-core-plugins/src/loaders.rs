//! Fabric / NeoForge / Fabric API 的远程目录。

use std::collections::BTreeMap;
use std::time::Duration;

use serde_json::Value;

use kaleid_api::{
    ApiError, ApiResult, FabricApiInfo, LoaderVersion, LoaderVersionTimes, LoaderVersions,
};

use crate::remote::{now_secs, RemoteClient};

const FABRIC_META: &str = "https://meta.fabricmc.net/v2";
const FABRIC_META_BMCLAPI: &str = "https://bmclapi2.bangbang93.com/fabric-meta/v2";
const FORGE_MAVEN: &str = "https://maven.minecraftforge.net";
const FORGE_PROMOTIONS: &str =
    "https://files.minecraftforge.net/net/minecraftforge/forge/promotions_slim.json";
const NEOFORGE_MAVEN: &str = "https://maven.neoforged.net";
const NEOFORGE_API: &str =
    "https://maven.neoforged.net/api/maven/versions/releases/net/neoforged/neoforge";
const BMCLAPI_MAVEN: &str = "https://bmclapi2.bangbang93.com/maven";
/// OptiFine 的列表与直链都在 BMCLAPI 上（官网要过 adfocus 跳转，抓不动）。
const BMCLAPI_OPTIFINE: &str = "https://bmclapi2.bangbang93.com/optifine";
const MODRINTH: &str = "https://api.modrinth.com/v2";

pub fn fabric_loaders(client: &RemoteClient, game: &str) -> ApiResult<LoaderVersions> {
    // Fabric 官方 meta 在部分网络下不可达；BMCLAPI 的 fabric-meta 路径
    // 返回同样的结构，优先走它，失败再回官方。
    let path = format!("/versions/loader/{}", url_encode(game));
    let value = client
        .get_json(&format!("{FABRIC_META_BMCLAPI}{path}"))
        .or_else(|_| client.get_json(&format!("{FABRIC_META}{path}")))?;
    let rows = value
        .as_array()
        .ok_or_else(|| ApiError::Format("Fabric loader 列表不是数组".into()))?;
    let mut versions = Vec::with_capacity(rows.len());
    for row in rows {
        let loader = row.get("loader").unwrap_or(&Value::Null);
        let version = loader
            .get("version")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        if version.is_empty() {
            continue;
        }
        let stable = loader
            .get("stable")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        versions.push(LoaderVersion {
            version,
            stable,
            recommended: false,
            time: None,
        });
    }
    let recommended = versions
        .iter()
        .find(|v| v.stable)
        .map(|v| v.version.clone())
        .or_else(|| versions.first().map(|v| v.version.clone()));
    for v in &mut versions {
        v.recommended = recommended.as_deref() == Some(v.version.as_str());
    }
    Ok(LoaderVersions {
        game_version: game.to_string(),
        loader: "fabric".into(),
        versions,
        recommended,
        fetched_at: now_secs(),
    })
}

pub fn forge_loaders(client: &RemoteClient, game: &str) -> ApiResult<LoaderVersions> {
    let metadata = client
        .get_text(&format!(
            "{BMCLAPI_MAVEN}/net/minecraftforge/forge/maven-metadata.xml"
        ))
        .or_else(|_| {
            client.get_text(&format!(
                "{FORGE_MAVEN}/net/minecraftforge/forge/maven-metadata.xml"
            ))
        })?;
    let prefix = format!("{game}-");
    let mut matching: Vec<String> = xml_versions(&metadata)
        .into_iter()
        .filter_map(|artifact| artifact.strip_prefix(&prefix).map(|v| v.to_string()))
        .collect();
    matching.sort_by(|a, b| natural_cmp(a, b));
    matching.reverse();
    matching.dedup();

    // promotions_slim.json 是 Forge 官方推荐标记；拿不到时宁可退回最新正式版。
    let promos = client.get_json(FORGE_PROMOTIONS).ok();
    let promo = |kind: &str| {
        promos
            .as_ref()
            .and_then(|v| v.get("promos"))
            .and_then(|v| v.get(&format!("{game}-{kind}")))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    };
    let recommended = promo("recommended")
        .filter(|v| matching.iter().any(|m| m == v))
        .or_else(|| promo("latest").filter(|v| matching.iter().any(|m| m == v)))
        .or_else(|| {
            matching
                .iter()
                .find(|v| !is_test_version(v))
                .cloned()
                .or_else(|| matching.first().cloned())
        });
    let versions = matching
        .into_iter()
        .map(|version| LoaderVersion {
            stable: !is_test_version(&version),
            recommended: recommended.as_deref() == Some(version.as_str()),
            version,
            time: None,
        })
        .collect();
    Ok(LoaderVersions {
        game_version: game.to_string(),
        loader: "forge".into(),
        versions,
        recommended,
        fetched_at: now_secs(),
    })
}

/// Forge 元数据是 XML；这里只取 `<versions>` 下的 `<version>`，避免为一个简单列表引入 XML 依赖。
fn xml_versions(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find("<version>") {
        rest = &rest[start + "<version>".len()..];
        let Some(end) = rest.find("</version>") else {
            break;
        };
        let value = rest[..end].trim();
        if !value.is_empty() {
            out.push(value.to_string());
        }
        rest = &rest[end + "</version>".len()..];
    }
    out
}

pub fn neoforge_loaders(client: &RemoteClient, game: &str) -> ApiResult<LoaderVersions> {
    let all = neoforge_version_list(client)?;
    let mut matching = neoforge_matching(&all, game);
    // 官方 API 偶尔只返回当前分支；精确前缀筛选为空时再用镜像补一次。
    if matching.is_empty() {
        if let Ok(xml) = client.get_text(&neoforge_metadata_url()) {
            matching = neoforge_matching(&xml_versions(&xml), game);
        }
    }
    let recommended = matching
        .iter()
        .find(|v| !is_test_version(v))
        .cloned()
        .or_else(|| matching.first().cloned());
    let versions = matching
        .into_iter()
        .map(|version| LoaderVersion {
            stable: !is_test_version(&version),
            recommended: recommended.as_deref() == Some(version.as_str()),
            version,
            time: None,
        })
        .collect();
    Ok(LoaderVersions {
        game_version: game.to_string(),
        loader: "neoforge".into(),
        versions,
        recommended,
        fetched_at: now_secs(),
    })
}

/// 官方 Maven API 在部分网络下会被 CDN/DNS 拦截；优先读 BMCLAPI 的
/// `maven-metadata.xml`，失败再回官方 API。后者也包含全部 NeoForge 版本。
fn neoforge_version_list(client: &RemoteClient) -> ApiResult<Vec<String>> {
    let mut primary_error = None;
    match client.get_text(&neoforge_metadata_url()) {
        Ok(xml) => {
            let all = xml_versions(&xml);
            if !all.is_empty() {
                return Ok(all);
            }
        }
        Err(error) => primary_error = Some(error),
    }

    match client.get_json(NEOFORGE_API).and_then(|value| {
        value
            .get("versions")
            .and_then(|v| v.as_array())
            .map(|rows| {
                rows.iter()
                    .filter_map(|v| v.as_str().map(ToString::to_string))
                    .collect::<Vec<_>>()
            })
            .ok_or_else(|| ApiError::Format("NeoForge 版本列表缺少 versions".into()))
    }) {
        Ok(all) if !all.is_empty() => Ok(all),
        Ok(_) => {
            Err(primary_error.unwrap_or_else(|| ApiError::Format("NeoForge 版本列表为空".into())))
        }
        Err(error) => Err(primary_error.unwrap_or(error)),
    }
}

fn neoforge_metadata_url() -> String {
    format!("{BMCLAPI_MAVEN}/net/neoforged/neoforge/maven-metadata.xml")
}

fn neoforge_matching(all: &[String], game: &str) -> Vec<String> {
    let prefix = neoforge_prefix(game);
    let mut matching: Vec<String> = all
        .iter()
        .filter(|v| v.starts_with(&prefix))
        .cloned()
        .collect();
    matching.sort_by(|a, b| natural_cmp(a, b));
    matching.reverse();
    matching.dedup();
    matching
}

pub fn neoforge_prefix(game: &str) -> String {
    let parts: Vec<&str> = game.split('.').collect();
    if parts.len() >= 3 {
        // 旧版 1.x.y 的 NeoForge 前缀是 x.y.；2026 的 26.x.y 原样。
        if parts[0] == "1" {
            return format!("{}.{}.", parts[1], parts[2]);
        }
        return format!("{game}.");
    }
    if parts.len() == 2 {
        if parts[0] == "1" {
            return format!("{}.0.", parts[1]);
        }
        return format!("{}.0.", game);
    }
    format!("{game}.0.")
}

fn is_test_version(v: &str) -> bool {
    let lower = v.to_ascii_lowercase();
    lower.contains("-beta")
        || lower.contains("-alpha")
        || lower.contains("snapshot")
        || lower.contains("-pre")
}

pub fn fabric_profile_url(game: &str, loader: &str) -> String {
    format!(
        "{FABRIC_META}/versions/loader/{}/{}/profile/json",
        url_encode(game),
        url_encode(loader)
    )
}

pub fn fabric_profile(client: &RemoteClient, game: &str, loader: &str) -> ApiResult<Value> {
    // 与 `fabric_loaders` 一致：官方 meta 在部分网络下不可达，先走 BMCLAPI 的同一路径。
    let path = format!(
        "/versions/loader/{}/{}/profile/json",
        url_encode(game),
        url_encode(loader)
    );
    client
        .get_json(&format!("{FABRIC_META_BMCLAPI}{path}"))
        .or_else(|_| client.get_json(&fabric_profile_url(game, loader)))
}

pub fn neoforge_installer_url(version: &str) -> String {
    format!(
        "{NEOFORGE_MAVEN}/releases/net/neoforged/neoforge/{version}/neoforge-{version}-installer.jar"
    )
}

pub fn neoforge_installer_sha1_url(version: &str) -> String {
    format!("{}.sha1", neoforge_installer_url(version))
}

pub fn neoforge_installer_mirror_url(version: &str) -> String {
    format!("{BMCLAPI_MAVEN}/net/neoforged/neoforge/{version}/neoforge-{version}-installer.jar")
}

/// Forge 的 Maven 坐标把游戏版本和加载器版本拼在一起，例如 `1.21.4-54.1.22`。
pub fn forge_installer_url(game: &str, version: &str) -> String {
    let full = format!("{game}-{version}");
    format!("{FORGE_MAVEN}/net/minecraftforge/forge/{full}/forge-{full}-installer.jar")
}

pub fn forge_installer_sha1_url(game: &str, version: &str) -> String {
    format!("{}.sha1", forge_installer_url(game, version))
}

/// Forge 安装器的 BMCLAPI 镜像。官方 `maven.minecraftforge.net` 在国内经常连不上。
pub fn forge_installer_mirror_url(game: &str, version: &str) -> String {
    let full = format!("{game}-{version}");
    format!("{BMCLAPI_MAVEN}/net/minecraftforge/forge/{full}/forge-{full}-installer.jar")
}

/// `libraries/` 相对路径 → BMCLAPI Maven 镜像地址。
///
/// BMCLAPI 把 `maven.neoforged.net/releases`、`maven.minecraftforge.net`、
/// `maven.fabricmc.net`、`libraries.minecraft.net` 收在同一个 `/maven/` 下，
/// 路径就是 Maven 相对路径，不带 `releases/` 前缀。
pub fn bmclapi_maven_url(path: &str) -> String {
    format!(
        "{BMCLAPI_MAVEN}/{}",
        path.trim_start_matches('/').replace('\\', "/")
    )
}

// ---------------------------------------------------------------------------
// OptiFine
// ---------------------------------------------------------------------------

/// OptiFine 的 `version` 形如 `HD_U_J3`（类型 + 补丁号），目录列表就是这个形状。
///
/// BMCLAPI 的 `/optifine/<游戏版本>` 直接给出 OptiFine 官网那份列表：
/// `[{mcversion, type, patch, filename, forge}]`。它比 OptiFine 官网好抓
/// （官网要过 adfocus 跳转），也不依赖 Forge/NeoForge 那套 Maven。
pub fn optifine_versions(client: &RemoteClient, game: &str) -> ApiResult<LoaderVersions> {
    let value = client.get_json(&format!(
        "{BMCLAPI_OPTIFINE}/{}",
        url_encode(game)
    ))?;
    parse_optifine_list(game, &value)
}

/// 把 BMCLAPI 的 OptiFine 列表解析成 [`LoaderVersions`]（不碰网络，单测用）。
pub fn parse_optifine_list(game: &str, value: &Value) -> ApiResult<LoaderVersions> {
    let rows = value
        .as_array()
        .ok_or_else(|| ApiError::Format("OptiFine 版本列表格式不认识".into()))?;
    let mut versions = Vec::with_capacity(rows.len());
    for row in rows {
        let patch = row
            .get("patch")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        let kind = row.get("type").and_then(|v| v.as_str()).unwrap_or_default();
        if patch.is_empty() || kind.is_empty() {
            continue;
        }
        // `HD_U_J4_pre1` 这种预览版标记成 unstable，但仍列出来——老版本玩家经常
        // 就得靠预览版才能配合特定 Forge。
        let stable = !patch.to_ascii_lowercase().contains("pre");
        versions.push(LoaderVersion {
            version: format!("{kind}_{patch}"),
            stable,
            recommended: false,
            time: None,
        });
    }
    // BMCLAPI 的列表是**由旧到新**（1.12.2 第一条是 HD_U_C5）。和 Forge / NeoForge
    // 那几份保持一致，统一翻成"由新到旧"，界面默认选中第一条就是最新稳定版。
    versions.reverse();
    let recommended = versions
        .iter()
        .find(|v| v.stable)
        .map(|v| v.version.clone())
        .or_else(|| versions.first().map(|v| v.version.clone()));
    for v in &mut versions {
        v.recommended = recommended.as_deref() == Some(v.version.as_str());
    }
    Ok(LoaderVersions {
        game_version: game.to_string(),
        loader: "optifine".into(),
        versions,
        recommended,
        fetched_at: now_secs(),
    })
}

/// BMCLAPI 的 OptiFine 直链：`/optifine/<游戏版本>/<类型>/<补丁号>` 直接返回 jar。
pub fn optifine_download_url(game: &str, kind: &str, patch: &str) -> String {
    format!(
        "{BMCLAPI_OPTIFINE}/{}/{}/{}",
        url_encode(game),
        url_encode(kind),
        url_encode(patch)
    )
}

/// OptiFine 官网的备用地址（`download?f=<文件名>` 会 302 到镜像站）。
pub fn optifine_official_url(file_name: &str) -> String {
    format!("https://optifine.net/download?f={}", url_encode(file_name))
}

/// OptiFine jar 的文件名，官方/BMCLAPI 共用这一份命名。
pub fn optifine_jar_name(game: &str, version: &str) -> String {
    format!("OptiFine_{game}_{version}.jar")
}

/// OptiFine 在低版本单装时的库坐标（与 OptiFine 自己 build 出来的 profile 一致）。
pub fn optifine_library_coordinate(game: &str, version: &str) -> String {
    format!("optifine:OptiFine:{game}_{version}")
}

pub fn fabric_api_latest(client: &RemoteClient, game: &str) -> ApiResult<Option<FabricApiInfo>> {
    let query = format!(
        "{MODRINTH}/project/fabric-api/version?game_versions=%5B%22{}%22%5D&loaders=%5B%22fabric%22%5D",
        url_encode(game)
    );
    let value = client.get_json(&query)?;
    let rows = value
        .as_array()
        .ok_or_else(|| ApiError::Format("Modrinth 返回不是数组".into()))?;
    let chosen = rows
        .iter()
        .find(|v| v.get("version_type").and_then(|x| x.as_str()) == Some("release"))
        .or_else(|| rows.first());
    let Some(v) = chosen else {
        return Ok(None);
    };
    let version = v
        .get("version_number")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    let files = v.get("files").and_then(|x| x.as_array());
    let file = files
        .and_then(|files| {
            files
                .iter()
                .find(|f| f.get("primary").and_then(|p| p.as_bool()) == Some(true))
                .or_else(|| files.first())
        })
        .ok_or_else(|| ApiError::Format("Fabric API 版本没有文件".into()))?;
    let file_name = file
        .get("filename")
        .and_then(|x| x.as_str())
        .unwrap_or("fabric-api.jar")
        .to_string();
    let download_url = file
        .get("url")
        .and_then(|x| x.as_str())
        .ok_or_else(|| ApiError::Format("Fabric API 文件缺少 URL".into()))?
        .to_string();
    let size = file.get("size").and_then(|x| x.as_u64()).unwrap_or(0);
    let sha1 = file
        .get("hashes")
        .and_then(|h| h.get("sha1"))
        .and_then(|x| x.as_str())
        .map(|s| s.to_string());
    Ok(Some(FabricApiInfo {
        version,
        file_name,
        download_url,
        size,
        sha1,
    }))
}

pub fn loader_times(
    client: &RemoteClient,
    game: &str,
    loader: &str,
    versions: &[String],
) -> ApiResult<LoaderVersionTimes> {
    let mut times = BTreeMap::new();
    for version in versions.iter().take(20) {
        let urls = loader_pom_urls(game, loader, version)?;
        let time = urls.iter().find_map(|url| head_last_modified(client, url));
        times.insert(version.clone(), time);
    }
    Ok(LoaderVersionTimes { times })
}

fn loader_pom_urls(game: &str, loader: &str, version: &str) -> ApiResult<Vec<String>> {
    let urls = match loader {
        "fabric" => vec![format!(
            "https://maven.fabricmc.net/net/fabricmc/fabric-loader/{version}/fabric-loader-{version}.pom"
        )],
        "forge" => vec![
            format!(
                "{FORGE_MAVEN}/net/minecraftforge/forge/{game}-{version}/forge-{game}-{version}.pom"
            ),
            format!(
                "{BMCLAPI_MAVEN}/net/minecraftforge/forge/{game}-{version}/forge-{game}-{version}.pom"
            ),
        ],
        "neoforge" => vec![
            format!(
                "{BMCLAPI_MAVEN}/net/neoforged/neoforge/{version}/neoforge-{version}.pom"
            ),
            format!(
                "{NEOFORGE_MAVEN}/releases/net/neoforged/neoforge/{version}/neoforge-{version}.pom"
            ),
        ],
        _ => return Err(ApiError::invalid("未知加载器")),
    };
    Ok(urls)
}

fn head_last_modified(client: &RemoteClient, url: &str) -> Option<String> {
    let response = client
        .agent()
        .head(url)
        .timeout(Duration::from_secs(8))
        .call()
        .or_else(|_| {
            client
                .agent()
                .get(url)
                .timeout(Duration::from_secs(8))
                .call()
        })
        .ok()?;
    response.header("Last-Modified").map(|s| s.to_string())
}

/// 从 Maven 坐标推导相对路径。
pub fn maven_path(name: &str) -> Option<String> {
    let mut parts = name.split(':');
    let group = parts.next()?;
    let artifact = parts.next()?;
    let version = parts.next()?;
    let classifier = parts.next();
    let ext = parts.next().unwrap_or("jar");
    let file = match classifier {
        Some(c) if !c.is_empty() => format!("{artifact}-{version}-{c}.{ext}"),
        _ => format!("{artifact}-{version}.{ext}"),
    };
    Some(format!(
        "{}/{artifact}/{version}/{file}",
        group.replace('.', "/")
    ))
}

/// 从坐标 + 仓库根生成下载地址。
pub fn maven_url(base: &str, name: &str) -> Option<String> {
    let path = maven_path(name)?;
    Some(format!("{}/{}", base.trim_end_matches('/'), path))
}

pub fn url_encode(input: &str) -> String {
    let mut out = String::new();
    for byte in input.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            b' ' => out.push_str("%20"),
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    let split = |s: &str| -> Vec<String> {
        s.split(|c: char| c == '.' || c == '-' || c == '_')
            .map(|p| p.to_string())
            .collect()
    };
    let pa = split(a);
    let pb = split(b);
    for i in 0..pa.len().max(pb.len()) {
        let sa = pa.get(i).cloned().unwrap_or_default();
        let sb = pb.get(i).cloned().unwrap_or_default();
        let ord = match (sa.parse::<u64>(), sb.parse::<u64>()) {
            (Ok(x), Ok(y)) => x.cmp(&y),
            _ => sa.cmp(&sb),
        };
        if ord != std::cmp::Ordering::Equal {
            return ord;
        }
    }
    std::cmp::Ordering::Equal
}

#[cfg(test)]
mod tests {
    use super::*;

    /// BMCLAPI 的 OptiFine 列表形状：类型与补丁号分开给，拼成 `HD_U_J3`。
    #[test]
    fn optifine_list_parses_bmclapi_shape() {
        let rows = serde_json::json!([
            {"mcversion": "1.21.4", "type": "HD_U", "patch": "J3",
             "filename": "OptiFine_1.21.4_HD_U_J3.jar", "forge": "Forge 54.0.34"},
            {"mcversion": "1.21.4", "type": "HD_U", "patch": "J4_pre1",
             "filename": "OptiFine_1.21.4_HD_U_J4_pre1.jar", "forge": ""}
        ]);
        let list = parse_optifine_list("1.21.4", &rows).unwrap();

        assert_eq!(list.loader, "optifine");
        assert_eq!(list.game_version, "1.21.4");
        let ids: Vec<&str> = list.versions.iter().map(|v| v.version.as_str()).collect();
        // BMCLAPI 给的是由旧到新，返回给界面的统一是由新到旧。
        assert_eq!(ids, vec!["HD_U_J4_pre1", "HD_U_J3"]);
        assert!(!list.versions[0].stable, "预览版不能标成稳定版");
        assert!(list.versions[1].stable);
        assert_eq!(list.recommended.as_deref(), Some("HD_U_J3"));
        assert!(list.versions[1].recommended);

        // 地址与文件名要和上面拼出来的 version 对得上。
        assert_eq!(
            optifine_download_url("1.12.2", "HD_U", "G5"),
            "https://bmclapi2.bangbang93.com/optifine/1.12.2/HD_U/G5"
        );
        assert_eq!(
            optifine_jar_name("1.12.2", "HD_U_G5"),
            "OptiFine_1.12.2_HD_U_G5.jar"
        );
        assert_eq!(
            optifine_library_coordinate("1.12.2", "HD_U_G5"),
            "optifine:OptiFine:1.12.2_HD_U_G5"
        );
        assert!(optifine_official_url("OptiFine_1.12.2_HD_U_G5.jar")
            .starts_with("https://optifine.net/download?f="));
    }

    #[test]
    fn parses_forge_maven_versions_and_builds_installer_url() {
        let xml = "<metadata><versioning><versions><version>1.21.4-54.1.22</version><version>26.1.2-64.1.5</version></versions></versioning></metadata>";
        assert_eq!(
            xml_versions(xml),
            vec!["1.21.4-54.1.22".to_string(), "26.1.2-64.1.5".to_string()]
        );
        assert_eq!(
            forge_installer_url("1.21.4", "54.1.22"),
            "https://maven.minecraftforge.net/net/minecraftforge/forge/1.21.4-54.1.22/forge-1.21.4-54.1.22-installer.jar"
        );
    }

    #[test]
    fn neoforge_prefix_maps_new_and_old_schemes() {
        assert_eq!(neoforge_prefix("26.1.2"), "26.1.2.");
        assert_eq!(neoforge_prefix("26.1"), "26.1.0.");
        assert_eq!(neoforge_prefix("26.2"), "26.2.0.");
        assert_eq!(neoforge_prefix("1.21.4"), "21.4.");
        assert_eq!(neoforge_prefix("1.21"), "21.0.");
        assert_eq!(neoforge_prefix("1.20.2"), "20.2.");
    }

    #[test]
    fn maven_path_handles_classifiers_and_extension() {
        assert_eq!(
            maven_path("net.fabricmc:fabric-loader:0.16.9").as_deref(),
            Some("net/fabricmc/fabric-loader/0.16.9/fabric-loader-0.16.9.jar")
        );
        assert_eq!(
            maven_path("net.neoforged:mergetool:2.0.7:api").as_deref(),
            Some("net/neoforged/mergetool/2.0.7/mergetool-2.0.7-api.jar")
        );
        assert_eq!(
            maven_path("a:b:1:win:txt").as_deref(),
            Some("a/b/1/b-1-win.txt")
        );
    }

    #[test]
    fn test_versions_are_recognized() {
        assert!(is_test_version("26.1.2.1-beta"));
        assert!(is_test_version("26.1.0.0-alpha.1+snapshot-1"));
        assert!(!is_test_version("26.1.2.125"));
    }

    #[test]
    fn neoforge_filters_year_based_game_versions() {
        let all = vec![
            "26.1.0.19-beta".to_string(),
            "26.1.0.18-beta".to_string(),
            "26.1.2.125".to_string(),
            "26.1.2.124".to_string(),
            "26.1.1.15-beta".to_string(),
            "26.2.0.93".to_string(),
            "26.2.0.92".to_string(),
            "26.3.0.99".to_string(),
            "21.1.80".to_string(),
        ];
        assert_eq!(
            neoforge_matching(&all, "26.1"),
            vec!["26.1.0.19-beta".to_string(), "26.1.0.18-beta".to_string()]
        );
        assert_eq!(
            neoforge_matching(&all, "26.1.1"),
            vec!["26.1.1.15-beta".to_string()]
        );
        assert_eq!(
            neoforge_matching(&all, "26.1.2"),
            vec!["26.1.2.125".to_string(), "26.1.2.124".to_string()]
        );
        assert_eq!(
            neoforge_matching(&all, "26.2"),
            vec!["26.2.0.93".to_string(), "26.2.0.92".to_string()]
        );
        assert_eq!(
            neoforge_matching(&all, "26.3"),
            vec!["26.3.0.99".to_string()]
        );
    }
}
