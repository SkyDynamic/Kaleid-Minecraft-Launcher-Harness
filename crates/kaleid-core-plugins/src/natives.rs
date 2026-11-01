//! 老版本（LWJGL 2 时代）的原生库。
//!
//! 1.5.2 这类版本的 JSON 把原生库写在
//! `downloads.classifiers["natives-windows"]` 里，同时在 `natives` 里点名当前
//! 平台用哪个分类器，另外带一份 `extract.exclude` 说明解压时要跳过什么：
//!
//! ```json
//! {"name": "org.lwjgl.lwjgl:lwjgl-platform:2.9.0",
//!  "natives": {"windows": "natives-windows"},
//!  "extract": {"exclude": ["META-INF/"]},
//!  "downloads": {"classifiers": {"natives-windows": {"path": "...", "url": "..."}}}}
//! ```
//!
//! 只看 `downloads.artifact` 的代码会把这类库整条漏掉，于是版本目录里没有
//! natives jar，`natives/` 是空的，`-Djava.library.path` 指了个空目录，游戏一
//! 启动就 `java.lang.UnsatisfiedLinkError: no lwjgl in java.library.path`。
//!
//! 1.13+（LWJGL 3）换了写法：原生包本身就是一条普通库（`name` 带
//! `:natives-windows`，走 `downloads.artifact`），会正常进 classpath。所以这里
//! 只负责"分类器"这一种形状，LWJGL 3 那条路保持不变。

use serde_json::Value;

/// Mojang 的库 CDN。分类器没带 `url` 时自己拼（老 Forge profile 常见）。
const LIBRARIES_BASE: &str = "https://libraries.minecraft.net";

/// 当前平台在 Mojang JSON 里的名字。
pub(crate) fn current_os() -> &'static str {
    if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "macos") {
        "osx"
    } else {
        "linux"
    }
}

/// 一个需要下载、解压的原生包。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NativeJar {
    /// `libraries/` 下的相对路径（正斜杠，Maven 布局）。
    pub path: String,
    pub url: String,
    pub sha1: Option<String>,
    pub size: u64,
    /// `extract.exclude` 里的前缀，例如 `META-INF/`。
    pub excludes: Vec<String>,
}

/// 取出一条库在当前平台需要下载的原生包。不认识或与平台无关时返回空。
pub(crate) fn native_jars(lib: &Value, os: &str) -> Vec<NativeJar> {
    let excludes = lib
        .get("extract")
        .and_then(|extract| extract.get("exclude"))
        .and_then(|exclude| exclude.as_array())
        .map(|list| {
            list.iter()
                .filter_map(|item| item.as_str())
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    let mut keys: Vec<String> = Vec::new();
    // ① `natives` 里按平台点名的分类器：老版本（1.5.x / 1.6.x）唯一的来源。
    if let Some(natives) = lib.get("natives").and_then(|value| value.as_object()) {
        if let Some(entry) = natives.get(os) {
            push_classifier_names(entry, &mut keys);
        }
    }
    // ② 有些 profile 只给 `downloads.classifiers`，那就靠键名认平台。
    if let Some(classifiers) = lib
        .get("downloads")
        .and_then(|downloads| downloads.get("classifiers"))
        .and_then(|classifiers| classifiers.as_object())
    {
        let needle = format!("natives-{os}");
        for key in classifiers.keys() {
            if key.contains(&needle) {
                keys.push(key.clone());
            }
        }
    }
    let mut seen = std::collections::HashSet::new();
    keys.retain(|key| seen.insert(key.clone()));

    let name = lib
        .get("name")
        .and_then(|value| value.as_str())
        .unwrap_or_default();
    let mut jars = Vec::new();
    for key in keys {
        // `${arch}` 之类的占位符没法在这里可靠展开（Forge 自己会替换），
        // 硬猜出来的路径只会 404，所以直接跳过。
        if key.contains("${") {
            continue;
        }
        let download = lib
            .get("downloads")
            .and_then(|downloads| downloads.get("classifiers"))
            .and_then(|classifiers| classifiers.get(&key));
        let path = download
            .and_then(|item| item.get("path"))
            .and_then(|value| value.as_str())
            .map(str::to_string)
            .or_else(|| maven_native_path(name, &key));
        let Some(path) = path else {
            continue;
        };
        let url = download
            .and_then(|item| item.get("url"))
            .and_then(|value| value.as_str())
            .map(str::to_string)
            .unwrap_or_else(|| format!("{LIBRARIES_BASE}/{path}"));
        jars.push(NativeJar {
            path,
            url,
            sha1: download
                .and_then(|item| item.get("sha1"))
                .and_then(|value| value.as_str())
                .map(str::to_string),
            size: download
                .and_then(|item| item.get("size"))
                .and_then(|value| value.as_u64())
                .unwrap_or(0),
            excludes: excludes.clone(),
        });
    }
    jars
}

/// `natives` 的取值可能是字符串，也可能是字符串数组（老 Forge 写法）。
fn push_classifier_names(entry: &Value, out: &mut Vec<String>) {
    if let Some(single) = entry.as_str() {
        out.push(single.to_string());
        return;
    }
    if let Some(list) = entry.as_array() {
        for item in list {
            if let Some(single) = item.as_str() {
                out.push(single.to_string());
            }
        }
    }
}

/// `group:artifact:version` + 分类器 → `libraries/` 下的 Maven 相对路径。
fn maven_native_path(name: &str, classifier: &str) -> Option<String> {
    let mut parts = name.split(':');
    let group = parts.next()?;
    let artifact = parts.next()?;
    let version = parts.next()?;
    if group.is_empty() || artifact.is_empty() || version.is_empty() {
        return None;
    }
    let group_path = group.replace('.', "/");
    Some(format!(
        "{group_path}/{artifact}/{version}/{artifact}-{version}-{classifier}.jar"
    ))
}

/// 解压时这个条目要不要写进 `natives/`。
pub(crate) fn native_entry_allowed(entry: &str, excludes: &[String]) -> bool {
    let entry = entry.replace('\\', "/");
    let entry = entry.trim_start_matches('/');
    if entry.is_empty() {
        return false;
    }
    let lower = entry.to_ascii_lowercase();
    // `extract.exclude` 里给的是目录前缀（例如 `META-INF/`），按前缀挡掉。
    !excludes.iter().any(|exclude| {
        let exclude = exclude.replace('\\', "/").to_ascii_lowercase();
        !exclude.is_empty() && lower.starts_with(&exclude)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 1.5.2 的真实形状：`natives` 点名分类器，`downloads.classifiers` 给地址。
    #[test]
    fn mojang_1_5_2_natives_are_classifier_jars() {
        let lib = json!({
            "name": "org.lwjgl.lwjgl:lwjgl-platform:2.9.0",
            "natives": {"linux": "natives-linux", "osx": "natives-osx", "windows": "natives-windows"},
            "extract": {"exclude": ["META-INF/"]},
            "downloads": {
                "classifiers": {
                    "natives-windows": {
                        "path": "org/lwjgl/lwjgl/lwjgl-platform/2.9.0/lwjgl-platform-2.9.0-natives-windows.jar",
                        "url": "https://libraries.minecraft.net/org/lwjgl/lwjgl/lwjgl-platform/2.9.0/lwjgl-platform-2.9.0-natives-windows.jar",
                        "size": 578680,
                        "sha1": "4c517ded644ac1333c1cbfcb8d2a8a56c8a1a1f9"
                    }
                }
            }
        });

        let windows = native_jars(&lib, "windows");
        assert_eq!(windows.len(), 1, "Windows 上应该只有一个原生包");
        let jar = &windows[0];
        assert_eq!(
            jar.path,
            "org/lwjgl/lwjgl/lwjgl-platform/2.9.0/lwjgl-platform-2.9.0-natives-windows.jar"
        );
        assert_eq!(
            jar.url,
            "https://libraries.minecraft.net/org/lwjgl/lwjgl/lwjgl-platform/2.9.0/lwjgl-platform-2.9.0-natives-windows.jar"
        );
        assert_eq!(jar.size, 578680);
        assert!(jar.sha1.is_some());
        assert_eq!(jar.excludes, vec!["META-INF/".to_string()]);

        let linux = native_jars(&lib, "linux");
        assert_eq!(
            linux[0].path,
            "org/lwjgl/lwjgl/lwjgl-platform/2.9.0/lwjgl-platform-2.9.0-natives-linux.jar",
            "分类器缺 path 时要按 Maven 坐标推出来"
        );
    }

    /// 没有 `downloads` 的老 Forge profile：只给坐标 + `natives`，也要算得出来。
    #[test]
    fn legacy_forge_natives_without_downloads_derive_maven_path() {
        let lib = json!({
            "name": "org.lwjgl.lwjgl:lwjgl-platform:2.9.1",
            "natives": {"windows": "natives-windows"}
        });
        let jars = native_jars(&lib, "windows");
        assert_eq!(jars.len(), 1);
        assert_eq!(
            jars[0].path,
            "org/lwjgl/lwjgl/lwjgl-platform/2.9.1/lwjgl-platform-2.9.1-natives-windows.jar"
        );
        assert_eq!(
            jars[0].url,
            "https://libraries.minecraft.net/org/lwjgl/lwjgl/lwjgl-platform/2.9.1/lwjgl-platform-2.9.1-natives-windows.jar"
        );
        assert!(jars[0].sha1.is_none());
        assert!(jars[0].excludes.is_empty());
    }

    /// 只给 `downloads.classifiers` 不给 `natives`：靠键名认平台。
    #[test]
    fn classifier_keys_alone_are_enough() {
        let lib = json!({
            "name": "org.lwjgl.lwjgl:jinput-platform:2.0.5",
            "downloads": {"classifiers": {
                "natives-windows": {"path": "org/lwjgl/lwjgl/jinput-platform/2.0.5/jinput-platform-2.0.5-natives-windows.jar"},
                "natives-linux": {"path": "org/lwjgl/lwjgl/jinput-platform/2.0.5/jinput-platform-2.0.5-natives-linux.jar"}
            }}
        });
        let jars = native_jars(&lib, "windows");
        assert_eq!(jars.len(), 1);
        assert!(jars[0].path.ends_with("jinput-platform-2.0.5-natives-windows.jar"));
        assert!(native_jars(&lib, "osx").is_empty(), "没有 osx 分类器就不该硬塞");
    }

    /// 与平台无关的库（纯 Java 依赖）不该被当成原生包。
    #[test]
    fn plain_libraries_have_no_natives() {
        let lib = json!({
            "name": "com.google.guava:guava:17.0",
            "downloads": {"artifact": {"path": "com/google/guava/guava/17.0/guava-17.0.jar"}}
        });
        assert!(native_jars(&lib, "windows").is_empty());
    }

    /// `META-INF/` 这类排除规则要生效，动态库本身要留下。
    #[test]
    fn extract_excludes_meta_inf() {
        let excludes = vec!["META-INF/".to_string()];
        assert!(!native_entry_allowed("META-INF/MANIFEST.MF", &excludes));
        assert!(native_entry_allowed("lwjgl.dll", &excludes));
        assert!(native_entry_allowed("windows/x64/OpenAL64.dll", &excludes));
        assert!(native_entry_allowed("a.dll", &[]));
    }
}
