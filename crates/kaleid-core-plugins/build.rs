//! 把仓库根 `build.env` 里的 `KEY=VALUE` 注入成编译期环境变量。
//!
//! 目的只有一个：**这些值不该出现在任何 `.rs` 里**。目前用到两处——
//! UA 版本号（MCIM 的白名单格式是 `KMLH/{version_code}`）与微软登录的
//! `client_id`（Azure 应用注册的公共客户端 ID）。改它们不该牵动源码，
//! 也不该要求每个开发者去改代码。
//!
//! 真实环境变量优先于文件，方便 CI 或本地临时覆盖；文件缺失时 version_code
//! 回落到 crate 版本号，保证编译不会因为少一份配置文件就失败（`client_id`
//! 缺失由运行时的 `option_env!` 处理，登录入口会明确提示去补）。

use std::collections::BTreeMap;
use std::path::PathBuf;

fn main() {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default());
    let env_file = manifest.join("../../build.env");
    println!("cargo:rerun-if-changed={}", env_file.display());
    println!("cargo:rerun-if-changed=build.rs");

    let mut vars: BTreeMap<String, String> = BTreeMap::new();
    if let Ok(text) = std::fs::read_to_string(&env_file) {
        parse_env(&text, &mut vars);
    }

    let version_code = pick("KMLH_VERSION_CODE", &vars)
        .unwrap_or_else(|| std::env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.0.0".into()));
    println!(
        "cargo:rustc-env=KMLH_VERSION_CODE={}",
        sanitize(&version_code)
    );

    // client_id 只是 GUID：缺就不注入（`option_env!` 会看到 None），
    // 但**不要**在这里顺手塞一个默认值——那会让人以为登录是通的。
    if let Some(raw) = pick("KALEID_MSA_CLIENT_ID", &vars) {
        match guid(raw.trim()) {
            Some(client_id) => println!("cargo:rustc-env=KALEID_MSA_CLIENT_ID={client_id}"),
            None => println!(
                "cargo:warning=build.env 里的 KALEID_MSA_CLIENT_ID 不是 GUID（`{}`），已忽略；微软登录会显示「还没配置 client_id」",
                raw.trim()
            ),
        }
    }
}

/// 真实环境变量 > build.env 文件；空值视为没填。
fn pick(key: &str, vars: &BTreeMap<String, String>) -> Option<String> {
    std::env::var(key)
        .ok()
        .filter(|v| !v.trim().is_empty())
        .or_else(|| vars.get(key).cloned())
        .filter(|v| !v.trim().is_empty())
}

fn parse_env(text: &str, out: &mut BTreeMap<String, String>) {
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if key.is_empty() || !key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            continue;
        }
        out.insert(key.to_string(), value.trim().trim_matches('"').to_string());
    }
}

/// 版本号会进 HTTP 头，只允许安全字符，避免把一个坏配置变成请求头注入。
fn sanitize(raw: &str) -> String {
    let cleaned: String = raw
        .trim()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '+'))
        .collect();
    if cleaned.is_empty() {
        "0.0.0".to_string()
    } else {
        cleaned
    }
}

/// 只接受 `8-4-4-4-12` 的十六进制 GUID。
///
/// 这里**不做**"尽力修补"：把一个打错的值过滤成另一个合法字符串，会让微软那边
/// 报 `invalid_client`，用户还得回头猜是哪个环节错了。宁可现在就报编译警告。
fn guid(raw: &str) -> Option<String> {
    let cleaned: String = raw
        .trim()
        .trim_matches(|c| c == '"' || c == '\'')
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    let shape = [8usize, 4, 4, 4, 12];
    let parts: Vec<&str> = cleaned.split('-').collect();
    if parts.len() != shape.len() {
        return None;
    }
    let ok = parts
        .iter()
        .zip(shape)
        .all(|(part, len)| part.len() == len && part.chars().all(|c| c.is_ascii_hexdigit()));
    if ok {
        Some(cleaned.to_ascii_lowercase())
    } else {
        None
    }
}
