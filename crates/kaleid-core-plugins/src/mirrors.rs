//! BMCLAPI 主站与**校园镜像**：同一个文件可以有九个地址。
//!
//! BMCLAPI 在国内是唯一像样的 Minecraft 文件源，但它自己也只有一台入口：
//! `bmclapi2.bangbang93.com` 实测带 `X-RateLimit-Limit-short: 10 / 10s` 的配额，
//! 一次原版安装 4000+ 个文件，撞 429 是常态。教育网的一批镜像站把它的**静态
//! 文件树**同步了出去，于是同一个文件可以按顺序试九个地址。
//!
//! 2026-11-02 实测（本机直连，非校园网）：
//!
//! | 端点 | `bmclapi2.bangbang93.com` | 校园镜像 |
//! |---|---|---|
//! | `/maven/<坐标>` | 200 | 主站的 `/maven/X` ≡ 镜像的 `/bmclapi/X`（**要剥掉 `/maven`**）；`maven-metadata.xml` 与 jar 的字节与主站一致（`java-objc-bridge-1.1.jar` 1330045 B，两边 SHA-1 都是 `1227f9e0…7d7b`） |
//! | `/assets/<p>/<h>` | 200 真内容 | `/bmclapi/assets/<p>/<h>` 回 **200 但是 0 字节**（占位文件） |
//! | `/version/<id>/json`、`/version/<id>/client` | 200 / 302 | **404**（不镜像） |
//! | `/mc/game/…`、`/optifine/…`、`/fabric-meta/…` | 200 | **404**（不镜像） |
//!
//! 覆盖范围也不是"主站有什么就有什么"：`forge-1.12.2-…-installer.jar` 在八家里只有
//! 一家给得出来，`org/lwjgl/lwjgl/3.3.3/lwjgl-3.3.3.jar` 却有七家给得出（785029 B，
//! SHA-1 `29589b5f…cb84`，与 Maven Central 逐字节相同）。所以它们是**兜底**而不是
//! 等价替换，顺序必须排在主站后面。
//!
//! 也正是这一天，主站把 `/maven/` 的请求 302 到 `chinanet.mirrors.ustc.edu.cn`，而
//! 那台对校园网外的来源回 403——一条请求链上两个节点各失败一次，本地七家镜像却都
//! 是好的。这正是这层兜底存在的理由。
//!
//! 也就是说校园镜像**只镜像静态文件树**，不是第二个 API 端点。所以它们不进
//! [`crate::plugins::builtin_sources`]（那张表要求有 `manifest_url`），只在下载器里
//! 作为"失败后轮询的候选地址"存在——见 `downloads::plan_urls`。
//!
//! 0 字节与 403 不需要特判：`downloads::finish_file` 会按大小 / SHA-1 判死，
//! `RetryClass::Content` 与 `Http` 的预算都是 0 次重试，直接换下一个地址。

use crate::urlcache::host_of;

/// BMCLAPI 主站。所有内置镜像 URL 都由它派生，换域名只改这一行。
pub const BMCLAPI_BASE: &str = "https://bmclapi2.bangbang93.com";
/// 主站主机名（限速记账、选源判定都按它）。
pub const BMCLAPI_HOST: &str = "bmclapi2.bangbang93.com";

pub const BMCLAPI_MANIFEST_URL: &str =
    "https://bmclapi2.bangbang93.com/mc/game/version_manifest_v2.json";
pub const BMCLAPI_MAVEN_BASE: &str = "https://bmclapi2.bangbang93.com/maven";
pub const BMCLAPI_OPTIFINE_BASE: &str = "https://bmclapi2.bangbang93.com/optifine";
pub const BMCLAPI_FABRIC_META: &str = "https://bmclapi2.bangbang93.com/fabric-meta/v2";
/// 这些地址都只在校园镜像"不镜像"的那一侧，留着是为了让 `plugins.rs` / `remote.rs`
/// 不用各自抄一遍字符串（单测里断言它们都以 [`BMCLAPI_BASE`] 开头）。
pub const BMCLAPI_STATIC_URLS: &[&str] = &[
    BMCLAPI_MANIFEST_URL,
    BMCLAPI_MAVEN_BASE,
    BMCLAPI_OPTIFINE_BASE,
    BMCLAPI_FABRIC_META,
];

/// 一条校园镜像。`base` 是它的 BMCLAPI 根（拼在 `/bmclapi` 后面）。
pub struct CampusMirror {
    pub host: &'static str,
    pub base: &'static str,
}

/// 教育网的 BMCLAPI 镜像，顺序就是失败后的重试顺序。
///
/// 2026-11-02 实测（同一个 jar，与本机是否在校园网内有关）：hit / nju / zju / ha /
/// qlu / nyist / sjtu 都返回逐字节相同的 785029 B；`mirrors.ustc.edu.cn` 对校园网外
/// 的来源回 403（主站把它当 CDN 节点，于是主站的请求也一起 403）。所以 ustc 留在
/// 队尾：真连不上只多花一次快速失败（`RetryClass::Http` 不重试）。
pub const CAMPUS_MIRRORS: &[CampusMirror] = &[
    CampusMirror {
        host: "mirrors.hit.edu.cn",
        base: "https://mirrors.hit.edu.cn/bmclapi",
    },
    CampusMirror {
        host: "mirror.nju.edu.cn",
        base: "https://mirror.nju.edu.cn/bmclapi",
    },
    CampusMirror {
        host: "mirrors.zju.edu.cn",
        base: "https://mirrors.zju.edu.cn/bmclapi",
    },
    CampusMirror {
        host: "mirrors.ha.edu.cn",
        base: "https://mirrors.ha.edu.cn/bmclapi",
    },
    CampusMirror {
        host: "mirrors.qlu.edu.cn",
        base: "https://mirrors.qlu.edu.cn/bmclapi",
    },
    CampusMirror {
        host: "mirror.nyist.edu.cn",
        base: "https://mirror.nyist.edu.cn/bmclapi",
    },
    CampusMirror {
        host: "mirror.sjtu.edu.cn",
        base: "https://mirror.sjtu.edu.cn/bmclapi",
    },
    CampusMirror {
        host: "mirrors.ustc.edu.cn",
        base: "https://mirrors.ustc.edu.cn/bmclapi",
    },
];

/// 去掉端口与大小写的裸主机名。
///
/// [`host_of`] 故意保留端口（限速记账要区分 `:8080` 和默认端口），但"这是不是
/// BMCLAPI"跟端口无关——`https://bmclapi2.bangbang93.com:443/…` 也是主站。
fn bare_host(host: &str) -> String {
    let host = host.trim().to_ascii_lowercase();
    match host.rsplit_once(':') {
        Some((head, port)) if !head.is_empty() && port.chars().all(|c| c.is_ascii_digit()) => {
            head.to_string()
        }
        _ => host,
    }
}

/// 这个地址是不是 BMCLAPI 主站上的（用户/插件给的第三方镜像不动它）。
pub fn is_bmclapi_url(url: &str) -> bool {
    bare_host(&host_of(url)) == BMCLAPI_HOST
}

/// 这个主机名是不是我们的某一条校园镜像。
pub fn is_campus_host(host: &str) -> bool {
    let host = bare_host(host);
    CAMPUS_MIRRORS.iter().any(|m| m.host == host)
}

/// 把主站上的路径搬到某个校园镜像上。
///
/// 规则只有一条，但很容易搞错：**校园镜像的根就是主站的 `/maven/`**。
/// 实测 `<base>/maven/<坐标>` 回 404，而 `<base>/<坐标>` 回 200；`/assets/…` 这类
/// 命名空间则原样保留（`<base>/assets/<p>/<h>`）。
pub fn to_campus(base: &str, path: &str) -> String {
    let base = base.trim_end_matches('/');
    if let Some(rest) = path.strip_prefix("/maven/") {
        format!("{base}/{rest}")
    } else if path == "/maven" || path.is_empty() {
        base.to_string()
    } else if path.starts_with('/') {
        format!("{base}{path}")
    } else {
        format!("{base}/{path}")
    }
}

/// 主站路径是不是"校园镜像也有一份"的那类。
///
/// 只有这两类：maven 坐标（依赖库 / 加载器 / 安装器）与资源对象。`/version/*`、
/// `/optifine/*`、`/fabric-meta/*`、`/mc/*` 实测在镜像上全是 404，列进去只会让
/// 每个失败的文件多花八次注定失败的请求。
fn is_mirrored_path(path: &str) -> bool {
    path.starts_with("/maven/") || path.starts_with("/assets/")
}

/// URL 里的路径部分（含开头的 `/`）；没有路径时是 `/`。
fn path_of(url: &str) -> &str {
    let rest = match url.split_once("://") {
        Some((_, rest)) => rest,
        None => url,
    };
    match rest.find(['/', '?', '#']) {
        Some(index) if rest.as_bytes()[index] == b'/' => &rest[index..],
        _ => "/",
    }
}

/// 一个下载地址展开成"同内容的候选地址组"。
///
/// - 不是 BMCLAPI 主站（MCIM、官方 CDN、插件自带的镜像）→ 原样一个；
/// - 是主站但路径不镜像（版本 JSON、客户端 jar、OptiFine）→ 原样一个；
/// - 是主站且路径镜像（maven / assets）→ `[主站, 8 个校园镜像]`。
///
/// 顺序即重试顺序：主站排第一，失败了才轮到校园。这正是 PCL2 / HMCL 的做法
/// ——镜像是兜底，不是分摊带宽。
pub fn expand_mirror(url: &str) -> Vec<String> {
    let mut urls = vec![url.to_string()];
    if !is_bmclapi_url(url) {
        return urls;
    }
    let path = path_of(url);
    if !is_mirrored_path(path) {
        return urls;
    }
    for mirror in CAMPUS_MIRRORS {
        let candidate = to_campus(mirror.base, path);
        if !urls.iter().any(|kept| kept == &candidate) {
            urls.push(candidate);
        }
    }
    urls
}

/// 官方地址 → BMCLAPI 等价地址。
///
/// PCL2 的做法是**只换主机、保留路径**（`DlSourceLauncherOrMetaGet`）：
/// `piston-meta.mojang.com/v1/packages/<sha>/<id>.json` 对应
/// `bmclapi2.bangbang93.com/v1/packages/<sha>/<id>.json`。这一点很关键——
/// 26.3 的 `/version/26.3/assetIndex` 会返回错误的大文件，而 `/v1/packages/...`
/// 是正确的小 JSON。
///
/// 只映射"路径能一一对上"的主机；`/version/*` 这种需要版本号上下文的旧路由
/// 不再由这里拼，调用方在极少数拿不到官方 URL 时才退回它。
pub fn bmclapi_for_official(official: &str) -> Option<String> {
    /// 前缀匹配后必须满足"剩余为空或以 `/` 开头"，防止
    /// `piston-meta.mojang.com.evil` 这类误匹配。
    fn rest_after<'a>(url: &'a str, prefix: &str) -> Option<&'a str> {
        let rest = url.strip_prefix(prefix)?;
        if rest.is_empty() || rest.starts_with('/') {
            Some(rest)
        } else {
            None
        }
    }

    // 最长/最具体的前缀先匹配：`files.minecraftforge.net/maven` 必须排在
    // `maven.minecraftforge.net` 之前，`maven.neoforged.net/releases` 同理。
    if let Some(rest) = rest_after(official, "https://files.minecraftforge.net/maven") {
        return Some(format!("{BMCLAPI_MAVEN_BASE}{rest}"));
    }
    if let Some(rest) = rest_after(official, "https://maven.neoforged.net/releases") {
        return Some(format!("{BMCLAPI_MAVEN_BASE}{rest}"));
    }
    for host in [
        "https://piston-meta.mojang.com",
        "https://piston-data.mojang.com",
        "https://launchermeta.mojang.com",
        "https://launcher.mojang.com",
    ] {
        if let Some(rest) = rest_after(official, host) {
            return Some(format!("{BMCLAPI_BASE}{rest}"));
        }
    }
    if let Some(rest) = rest_after(official, "https://resources.download.minecraft.net") {
        return Some(format!("{BMCLAPI_BASE}/assets{rest}"));
    }
    for host in [
        "https://libraries.minecraft.net",
        "https://maven.minecraftforge.net",
        "https://maven.neoforged.net",
        "https://maven.fabricmc.net",
    ] {
        if let Some(rest) = rest_after(official, host) {
            return Some(format!("{BMCLAPI_MAVEN_BASE}{rest}"));
        }
    }
    if let Some(rest) = rest_after(official, "https://meta.fabricmc.net/v2") {
        return Some(format!("{BMCLAPI_FABRIC_META}{rest}"));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_bmclapi_urls_all_hang_off_the_base() {
        for url in BMCLAPI_STATIC_URLS {
            assert!(
                url.starts_with(BMCLAPI_BASE),
                "{url} 应当以 {BMCLAPI_BASE} 开头"
            );
        }
        assert!(BMCLAPI_MANIFEST_URL.ends_with("/mc/game/version_manifest_v2.json"));
    }

    #[test]
    fn campus_bases_are_unique_https_and_bmclapi_rooted() {
        let mut hosts: Vec<&str> = Vec::new();
        for mirror in CAMPUS_MIRRORS {
            assert!(mirror.base.starts_with("https://"), "{} 必须走 HTTPS", mirror.base);
            assert!(
                mirror.base.ends_with("/bmclapi"),
                "{} 的根是 /bmclapi",
                mirror.base
            );
            assert_eq!(host_of(mirror.base), mirror.host, "host 与 base 要一致");
            assert!(!hosts.contains(&mirror.host), "{} 重复了", mirror.host);
            hosts.push(mirror.host);
        }
        assert_eq!(hosts.len(), 8, "用户点名的八家校园镜像");
    }

    #[test]
    fn campus_keeps_assets_but_strips_maven() {
        let base = "https://mirrors.hit.edu.cn/bmclapi";
        assert_eq!(
            to_campus(base, "/maven/net/minecraftforge/forge/1.20.1/forge.jar"),
            "https://mirrors.hit.edu.cn/bmclapi/net/minecraftforge/forge/1.20.1/forge.jar"
        );
        assert_eq!(
            to_campus(base, "/assets/7d/7dbc8bd8"),
            "https://mirrors.hit.edu.cn/bmclapi/assets/7d/7dbc8bd8"
        );
        // 主站根（`/maven`）落到镜像根。
        assert_eq!(to_campus(base, "/maven"), "https://mirrors.hit.edu.cn/bmclapi");
    }

    #[test]
    fn only_static_file_tree_gets_campus_alternates() {
        // maven：主站 + 八家。
        let group = expand_mirror("https://bmclapi2.bangbang93.com/maven/com/mojang/x/1/x-1.jar");
        assert_eq!(group.len(), 1 + CAMPUS_MIRRORS.len());
        assert_eq!(group[0], "https://bmclapi2.bangbang93.com/maven/com/mojang/x/1/x-1.jar");
        assert_eq!(group[1], "https://mirrors.hit.edu.cn/bmclapi/com/mojang/x/1/x-1.jar");
        assert!(group.iter().all(|url| !url.contains("/bmclapi/maven/")));

        // assets：同样保留命名空间。
        let group = expand_mirror("https://bmclapi2.bangbang93.com/assets/7d/7dbc8bd8");
        assert_eq!(group.len(), 1 + CAMPUS_MIRRORS.len());
        assert_eq!(group[1], "https://mirrors.hit.edu.cn/bmclapi/assets/7d/7dbc8bd8");

        // 版本 JSON / 客户端 jar / OptiFine / fabric-meta：镜像没有，只留主站。
        for url in [
            "https://bmclapi2.bangbang93.com/version/1.21.4/json",
            "https://bmclapi2.bangbang93.com/version/1.21.4/client",
            "https://bmclapi2.bangbang93.com/optifine/1.21.4/HD_U/J3",
            "https://bmclapi2.bangbang93.com/fabric-meta/v2/versions/loader/1.21.4",
            "https://bmclapi2.bangbang93.com/mc/game/version_manifest_v2.json",
        ] {
            assert_eq!(expand_mirror(url), vec![url.to_string()], "{url} 不该展开");
        }
    }

    #[test]
    fn third_party_mirrors_are_left_alone() {
        for url in [
            "https://mod.mcimirror.top/data/AANobbMI/x.jar",
            "https://resources.download.minecraft.net/7d/7dbc8bd8",
            "http://127.0.0.1:8080/maven/x.jar",
        ] {
            assert_eq!(expand_mirror(url), vec![url.to_string()], "{url} 不该展开");
        }
    }

    #[test]
    fn url_path_and_host_helpers() {
        assert_eq!(
            path_of("https://bmclapi2.bangbang93.com/maven/a/b.jar"),
            "/maven/a/b.jar"
        );
        assert_eq!(path_of("https://bmclapi2.bangbang93.com"), "/");
        assert_eq!(path_of("https://bmclapi2.bangbang93.com?x=1"), "/");
        assert!(is_bmclapi_url("https://BMCLAPI2.bangbang93.com:443/x"));
        assert!(!is_bmclapi_url("https://mirrors.hit.edu.cn/bmclapi/x"));
        assert!(is_campus_host("Mirrors.USTC.EDU.CN"));
        assert!(!is_campus_host("bmclapi2.bangbang93.com"));
    }

    #[test]
    fn official_addresses_map_onto_bmclapi_paths() {
        assert_eq!(
            bmclapi_for_official("https://resources.download.minecraft.net/7d/7dbc8bd8"),
            Some(format!("{BMCLAPI_BASE}/assets/7d/7dbc8bd8"))
        );
        assert_eq!(
            bmclapi_for_official("https://libraries.minecraft.net/com/mojang/patchy/2.2.10/patchy-2.2.10.jar"),
            Some(format!(
                "{BMCLAPI_MAVEN_BASE}/com/mojang/patchy/2.2.10/patchy-2.2.10.jar"
            ))
        );

        // PCL2 式主机替换：piston-meta 的 packages 路径就是 26.3 资源索引用到的
        // 那条地址。这是本次 16 MiB 报错的直接修复点。
        assert_eq!(
            bmclapi_for_official(
                "https://piston-meta.mojang.com/v1/packages/917ff6d74b9f635c46bac0d28857206bbcc1ffe4/34.json"
            ),
            Some(format!(
                "{BMCLAPI_BASE}/v1/packages/917ff6d74b9f635c46bac0d28857206bbcc1ffe4/34.json"
            ))
        );
        assert_eq!(
            bmclapi_for_official(
                "https://piston-data.mojang.com/v1/objects/abc/client.jar"
            ),
            Some(format!("{BMCLAPI_BASE}/v1/objects/abc/client.jar"))
        );
        assert_eq!(
            bmclapi_for_official(
                "https://piston-data.mojang.com/v1/objects/def/client-1.21.2.xml"
            ),
            Some(format!("{BMCLAPI_BASE}/v1/objects/def/client-1.21.2.xml"))
        );
        assert_eq!(
            bmclapi_for_official("https://launchermeta.mojang.com/mc/game/version_manifest_v2.json"),
            Some(format!("{BMCLAPI_BASE}/mc/game/version_manifest_v2.json"))
        );

        // 加载器 Maven：`/releases` 与 `/maven` 都要被剥成 BMCLAPI 的 `/maven/`。
        assert_eq!(
            bmclapi_for_official(
                "https://maven.neoforged.net/releases/net/neoforged/neoforge/26.3.0.101/neoforge-26.3.0.101-installer.jar"
            ),
            Some(format!(
                "{BMCLAPI_MAVEN_BASE}/net/neoforged/neoforge/26.3.0.101/neoforge-26.3.0.101-installer.jar"
            ))
        );
        assert_eq!(
            bmclapi_for_official(
                "https://files.minecraftforge.net/maven/net/minecraftforge/forge/1.21.1-52.0.0/forge-1.21.1-52.0.0-installer.jar"
            ),
            Some(format!(
                "{BMCLAPI_MAVEN_BASE}/net/minecraftforge/forge/1.21.1-52.0.0/forge-1.21.1-52.0.0-installer.jar"
            ))
        );
        assert_eq!(
            bmclapi_for_official(
                "https://maven.fabricmc.net/net/fabricmc/fabric-loader/0.16.9/fabric-loader-0.16.9.jar"
            ),
            Some(format!(
                "{BMCLAPI_MAVEN_BASE}/net/fabricmc/fabric-loader/0.16.9/fabric-loader-0.16.9.jar"
            ))
        );
        assert_eq!(
            bmclapi_for_official("https://meta.fabricmc.net/v2/versions/loader/1.21.4"),
            Some(format!("{BMCLAPI_FABRIC_META}/versions/loader/1.21.4"))
        );

        assert_eq!(bmclapi_for_official("https://cdn.modrinth.com/x.jar"), None);
        assert_eq!(
            bmclapi_for_official("https://piston-meta.mojang.com.evil/x.json"),
            None
        );
    }

    /// 真机用例：校园镜像到底还在不在同步。
    ///
    /// 联网，默认不跑：`cargo test -p kaleid-core-plugins --lib real_campus -- --ignored --nocapture`。
    /// 只打印每个镜像的字节数，不断言"八个都得通"——校园镜像会因为来源 IP、同步故障
    /// 临时挂掉，那是运行期行为，不是代码错误。但**至少三家**要能拿回真正的 jar 头
    /// （`PK\x03\x04`），否则"校园镜像"这个前提就不成立了。
    ///
    /// 探的是 LWJGL：2026-11-02 实测 hit / nju / zju / ha / qlu / nyist / sjtu 都回
    /// 785029 B、SHA-1 `29589b5f…cb84`，与 Maven Central 上的同一个文件逐字节相同；
    /// ustc 从校外 403。别换成 Forge 安装器那种冷门文件：实测
    /// `forge-1.12.2-…-installer.jar` 八家里只有一家有，探它会得出"镜像全挂了"的假结论
    /// ——这也正好说明它们只同步了常用的那部分静态文件树。
    #[test]
    #[ignore = "联网：验证教育网 BMCLAPI 镜像是否仍在同步"]
    fn real_campus_mirrors_serve_maven_files() {
        use std::io::Read;

        let path = "/maven/org/lwjgl/lwjgl/3.3.3/lwjgl-3.3.3.jar";
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(std::time::Duration::from_secs(8))
            .timeout_read(std::time::Duration::from_secs(20))
            .user_agent(crate::USER_AGENT)
            .build();
        let mut ok = 0usize;
        for url in expand_mirror(&format!("{BMCLAPI_BASE}{path}")) {
            // 只要头四个字节：是不是真 jar 一眼就能看出来，200 + 错误页骗不过去。
            let head = agent
                .get(&url)
                .set("Range", "bytes=0-3")
                .call()
                .map(|response| {
                    let mut head = [0u8; 4];
                    let _ = response.into_reader().read_exact(&mut head);
                    head
                })
                .unwrap_or_else(|e| {
                    println!("✗ {url}\n    {e}");
                    [0u8; 4]
                });
            let jar = &head == b"PK\x03\x04";
            if jar {
                ok += 1;
            }
            let verdict = if jar { "✓" } else { "✗" };
            println!(
                "{verdict} {:02x?} {}",
                head,
                url
            );
        }
        assert!(ok >= 3, "至少三家校园镜像应当能返回真正的 jar，实际 {ok}");
    }
}
