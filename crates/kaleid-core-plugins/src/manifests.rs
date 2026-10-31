//! 第三方声明式插件清单：解析、校验、安装与装载。
//!
//! 清单**不是代码**：动作是一个封闭集合，没有 `exec` / `eval` / `!!js`，也没有条件
//! 跳转。这样做的理由写在 `docs/plugins.md`：能跑代码的插件等于把整机权限交出去，
//! 而"加一条镜像源 / 加一块只读面板 / 改一个默认值"根本不需要那种能力。
//!
//! 安装位置固定是 `<数据目录>/plugins/<id>/manifest.json`。装载发生在内核 `load`
//! 之前（配置类动作要先改规格）与之后（服务类动作要等内核就绪）。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use kaleid_api::InstallSourceApi;
use kaleid_kernel::{merge_json, PluginSpec};

/// 宿主 API 主版本。清单里的 `api` 与之不符就拒绝装载。
pub const MANIFEST_API: &str = "1";
pub const MAX_ID_LEN: usize = 48;
const MAX_MANIFEST_BYTES: u64 = 256 * 1024;

/// 一份通过校验的清单。
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub id: String,
    pub name: String,
    pub version: String,
    pub api: String,
    #[serde(default)]
    pub author: Option<String>,
    /// 存在时会在安装阶段被检查；当前构建没有 ed25519 后端，带签名的清单会被拒绝。
    #[serde(default)]
    pub signature: Option<String>,
    pub steps: Vec<Step>,
}

/// 封闭的动作集合。
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "op", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Step {
    /// 深合并某个插件的配置，不替换整份。
    UpdateConfig { target: String, patch: Value },
    /// 停用某插件（必需插件会被拒绝）。
    DisablePlugin { target: String },
    /// 启用某插件。
    EnablePlugin { target: String },
    /// 再挂一层下载源，不顶掉内置源。
    ProvideSource {
        id: String,
        label: String,
        #[serde(default)]
        priority: i32,
        #[serde(rename = "manifestUrl")]
        manifest_url: String,
        #[serde(rename = "baseUrl")]
        base_url: String,
    },
    /// 往某个追加型插槽里加一块只读面板。
    BindSlot { slot: String, title: String, view: String },
    /// 只在用户没有显式设置时生效的默认值。
    SetDefaultSetting { key: String, value: Value },
}

/// 磁盘上一条已安装清单的现状（解析失败也要能看到，而不是静默消失）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstalledManifest {
    pub id: String,
    pub name: String,
    pub version: String,
    pub path: String,
    /// `ready` 或 `invalid`。
    pub state: String,
    pub error: Option<String>,
}

pub fn plugins_dir(data_root: &Path) -> PathBuf {
    data_root.join("plugins")
}

fn is_valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_ID_LEN
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// 校验一份已解析的清单。失败时给一句能直接展示给用户的话。
pub fn validate(manifest: &Manifest) -> Result<(), String> {
    if !is_valid_id(&manifest.id) {
        return Err(format!(
            "清单 id `{}` 不合法：只允许 a-z / 0-9 / -，长度不超过 {MAX_ID_LEN}",
            manifest.id
        ));
    }
    if manifest.name.trim().is_empty() {
        return Err("清单缺少展示名 name".into());
    }
    if manifest.version.trim().is_empty() {
        return Err("清单缺少 version".into());
    }
    if manifest.api != MANIFEST_API {
        return Err(format!(
            "清单要求的宿主 API 是 `{}`，当前只支持 `{MANIFEST_API}`",
            manifest.api
        ));
    }
    if manifest.steps.is_empty() {
        return Err("清单没有 steps，装了也不会做任何事".into());
    }
    for (index, step) in manifest.steps.iter().enumerate() {
        let at = index + 1;
        match step {
            Step::UpdateConfig { target, patch } => {
                if target.trim().is_empty() {
                    return Err(format!("第 {at} 步 update-config 缺少 target"));
                }
                if !patch.is_object() {
                    return Err(format!("第 {at} 步 update-config 的 patch 必须是对象"));
                }
            }
            Step::DisablePlugin { target } | Step::EnablePlugin { target } => {
                if target.trim().is_empty() {
                    return Err(format!("第 {at} 步缺少 target"));
                }
            }
            Step::ProvideSource {
                id,
                label,
                manifest_url,
                base_url,
                ..
            } => {
                if !is_valid_id(id) {
                    return Err(format!("第 {at} 步下载源 id `{id}` 不合法"));
                }
                if label.trim().is_empty() {
                    return Err(format!("第 {at} 步下载源缺少 label"));
                }
                for (field, url) in [("manifestUrl", manifest_url), ("baseUrl", base_url)] {
                    if !url.starts_with("https://") {
                        return Err(format!(
                            "第 {at} 步下载源的 {field} 必须是 https://（收到 `{url}`）"
                        ));
                    }
                }
            }
            Step::BindSlot { slot, title, view } => {
                if slot.trim().is_empty() || title.trim().is_empty() {
                    return Err(format!("第 {at} 步 bind-slot 缺少 slot 或 title"));
                }
                if !view.starts_with("readonly-text:") {
                    return Err(format!(
                        "第 {at} 步 bind-slot 的 view 只支持 `readonly-text:` 前缀：插件不能注入脚本或组件"
                    ));
                }
            }
            Step::SetDefaultSetting { key, .. } => {
                if key.trim().is_empty() {
                    return Err(format!("第 {at} 步 set-default-setting 缺少 key"));
                }
            }
        }
    }
    Ok(())
}

/// 从 JSON 文本解析并校验。
pub fn parse(text: &str) -> Result<Manifest, String> {
    let manifest: Manifest =
        serde_json::from_str(text).map_err(|e| format!("清单 JSON 结构不对：{e}"))?;
    validate(&manifest)?;
    Ok(manifest)
}

/// 把 `<source>` 解析成清单文件路径：可以给目录，也可以直接给 `manifest.json`。
fn manifest_file(source: &Path) -> Result<PathBuf, String> {
    if source.is_dir() {
        let file = source.join("manifest.json");
        if !file.is_file() {
            return Err(format!("{} 里没有 manifest.json", source.display()));
        }
        return Ok(file);
    }
    if source.is_file() {
        return Ok(source.to_path_buf());
    }
    Err(format!("找不到清单：{}", source.display()))
}

/// 安装一份清单。返回装好的那份，路径是它在数据目录里的落点。
///
/// `reserved` 是内置插件 id：第三方不能覆盖它们，否则「停用某插件」这类动作
/// 就能借同名清单改写内置行为。
pub fn install(
    data_root: &Path,
    source: &Path,
    allow_unsigned: bool,
    reserved: &[String],
) -> Result<(Manifest, PathBuf), String> {
    let file = manifest_file(source)?;
    let size = std::fs::metadata(&file)
        .map(|m| m.len())
        .map_err(|e| format!("{}: {e}", file.display()))?;
    if size > MAX_MANIFEST_BYTES {
        return Err(format!(
            "清单过大（{size} 字节，上限 {MAX_MANIFEST_BYTES}）"
        ));
    }
    let text = std::fs::read_to_string(&file).map_err(|e| format!("{}: {e}", file.display()))?;
    let manifest = parse(&text)?;
    if reserved.iter().any(|id| id == &manifest.id) {
        return Err(format!(
            "id `{}` 与内置插件重名，第三方清单不能覆盖内置插件",
            manifest.id
        ));
    }
    if manifest.signature.is_some() {
        return Err(
            "这份清单带 signature，但当前构建没有 ed25519 校验后端，无法验证。\
             请让作者提供未签名版本，或在本机确认来源后用 --allow-unsigned 安装。"
                .into(),
        );
    }
    if !allow_unsigned {
        return Err(
            "清单没有 signature，默认拒绝安装。确认来源可信后加 --allow-unsigned。".into(),
        );
    }
    let target_dir = plugins_dir(data_root).join(&manifest.id);
    std::fs::create_dir_all(&target_dir)
        .map_err(|e| format!("{}: {e}", target_dir.display()))?;
    let target = target_dir.join("manifest.json");
    std::fs::write(&target, serde_json::to_string_pretty(&manifest).unwrap_or(text))
        .map_err(|e| format!("{}: {e}", target.display()))?;
    Ok((manifest, target))
}

/// 列出已安装清单。解析失败的也列出来，带上原因。
pub fn list(data_root: &Path) -> Vec<InstalledManifest> {
    let dir = plugins_dir(data_root);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut out: Vec<InstalledManifest> = entries
        .flatten()
        .filter(|entry| entry.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .map(|entry| {
            let id = entry.file_name().to_string_lossy().to_string();
            let path = entry.path().join("manifest.json");
            let display = path.to_string_lossy().to_string();
            let parsed = std::fs::read_to_string(&path)
                .map_err(|e| format!("{}: {e}", path.display()))
                .and_then(|text| parse(&text));
            match parsed {
                Ok(manifest) => InstalledManifest {
                    id: manifest.id,
                    name: manifest.name,
                    version: manifest.version,
                    path: display,
                    state: "ready".into(),
                    error: None,
                },
                Err(error) => InstalledManifest {
                    id,
                    name: String::new(),
                    version: String::new(),
                    path: display,
                    state: "invalid".into(),
                    error: Some(error),
                },
            }
        })
        .collect();
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

/// 读全部**校验通过**的清单。
pub fn load_ready(data_root: &Path) -> Vec<Manifest> {
    let dir = plugins_dir(data_root);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut out: Vec<Manifest> = entries
        .flatten()
        .filter(|entry| entry.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .filter_map(|entry| {
            let path = entry.path().join("manifest.json");
            let text = std::fs::read_to_string(path).ok()?;
            parse(&text).ok()
        })
        .collect();
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

/// 卸载：只删 `<数据目录>/plugins/<id>`，内置插件不受影响。
pub fn remove(data_root: &Path, id: &str) -> Result<(), String> {
    if !is_valid_id(id) {
        return Err(format!("插件 id `{id}` 不合法"));
    }
    let dir = plugins_dir(data_root).join(id);
    if !dir.is_dir() {
        return Err(format!("没有安装 `{id}`"));
    }
    std::fs::remove_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))
}

/// 配置类动作必须在 `kernel.load` 之前作用到规格表上。
pub fn apply_to_specs(manifests: &[Manifest], specs: &mut [PluginSpec]) -> Result<(), String> {
    for manifest in manifests {
        for step in &manifest.steps {
            match step {
                Step::UpdateConfig { target, patch } => {
                    let Some(spec) = specs.iter_mut().find(|spec| &spec.id == target) else {
                        return Err(format!(
                            "清单 `{}` 的 update-config 指向不存在的插件 `{target}`",
                            manifest.id
                        ));
                    };
                    merge_json(&mut spec.config, patch);
                }
                Step::DisablePlugin { target } => {
                    let Some(spec) = specs.iter_mut().find(|spec| &spec.id == target) else {
                        return Err(format!(
                            "清单 `{}` 的 disable-plugin 指向不存在的插件 `{target}`",
                            manifest.id
                        ));
                    };
                    if spec.required {
                        return Err(format!(
                            "插件 `{target}` 是必需插件，清单 `{}` 不能停用它",
                            manifest.id
                        ));
                    }
                    spec.enabled = false;
                }
                Step::EnablePlugin { target } => {
                    let Some(spec) = specs.iter_mut().find(|spec| &spec.id == target) else {
                        return Err(format!(
                            "清单 `{}` 的 enable-plugin 指向不存在的插件 `{target}`",
                            manifest.id
                        ));
                    };
                    spec.enabled = true;
                }
                _ => {}
            }
        }
    }
    Ok(())
}

struct ManifestSource {
    id: String,
    label: String,
    priority: i32,
    manifest_url: String,
    base_url: String,
}

impl InstallSourceApi for ManifestSource {
    fn id(&self) -> &str {
        &self.id
    }
    fn label(&self) -> &str {
        &self.label
    }
    fn priority(&self) -> i32 {
        self.priority
    }
    fn manifest_url(&self) -> &str {
        &self.manifest_url
    }
    fn base_url(&self) -> &str {
        &self.base_url
    }
}

/// 一个清单贡献的下载源，打包成和内置源同样的表。
pub fn source_table(manifest: &Manifest) -> Option<(String, Vec<Arc<dyn InstallSourceApi>>)> {
    let mut sources: Vec<Arc<dyn InstallSourceApi>> = Vec::new();
    for step in &manifest.steps {
        if let Step::ProvideSource {
            id,
            label,
            priority,
            manifest_url,
            base_url,
        } = step
        {
            sources.push(Arc::new(ManifestSource {
                id: id.clone(),
                label: label.clone(),
                priority: *priority,
                manifest_url: manifest_url.clone(),
                base_url: base_url.clone(),
            }));
        }
    }
    if sources.is_empty() {
        None
    } else {
        Some((manifest.id.clone(), sources))
    }
}

/// 界面要渲染的只读面板。`slot` 是追加型插槽名，不替换任何内置界面。
pub fn slots(manifests: &[Manifest]) -> Vec<Value> {
    let mut out = Vec::new();
    for manifest in manifests {
        for step in &manifest.steps {
            if let Step::BindSlot { slot, title, view } = step {
                out.push(json!({
                    "plugin": manifest.id,
                    "slot": slot,
                    "title": title,
                    "text": view.trim_start_matches("readonly-text:"),
                }));
            }
        }
    }
    out
}

/// 清单声明的默认值。调用方负责「用户已经设置过就不覆盖」。
pub fn default_settings(manifests: &[Manifest]) -> Vec<(String, Value)> {
    let mut out = Vec::new();
    for manifest in manifests {
        for step in &manifest.steps {
            if let Step::SetDefaultSetting { key, value } = step {
                out.push((key.clone(), value.clone()));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(id: &str, steps: Vec<Step>) -> Manifest {
        Manifest {
            id: id.into(),
            name: "示例".into(),
            version: "1.0.0".into(),
            api: MANIFEST_API.into(),
            author: None,
            signature: None,
            steps,
        }
    }

    #[test]
    fn rejects_bad_ids_unknown_ops_and_insecure_urls() {
        assert!(validate(&sample("Bad_ID", vec![Step::DisablePlugin { target: "news".into() }])).is_err());
        assert!(validate(&sample("ok", vec![])).is_err());

        let insecure = sample(
            "ok",
            vec![Step::ProvideSource {
                id: "mirror".into(),
                label: "镜像".into(),
                priority: 1,
                manifest_url: "http://mirror.example/m.json".into(),
                base_url: "https://mirror.example".into(),
            }],
        );
        assert!(validate(&insecure).unwrap_err().contains("https://"));

        // 未知 op 在解析阶段就被拒。
        let text = r#"{"id":"ok","name":"n","version":"1.0.0","api":"1","steps":[{"op":"exec","cmd":"calc"}]}"#;
        assert!(parse(text).is_err());
    }

    #[test]
    fn rejects_script_bearing_slot_views() {
        let manifest = sample(
            "ok",
            vec![Step::BindSlot {
                slot: "overview.notices".into(),
                title: "面板".into(),
                view: "js:alert(1)".into(),
            }],
        );
        assert!(validate(&manifest).unwrap_err().contains("readonly-text"));
    }

    #[test]
    fn required_plugin_cannot_be_disabled_by_a_manifest() {
        let mut specs = vec![PluginSpec {
            id: "java-runtime".into(),
            name: "JavaRuntime".into(),
            enabled: true,
            config: json!({}),
            when: vec![],
            required: true,
        }];
        let manifest = sample(
            "evil",
            vec![Step::DisablePlugin {
                target: "java-runtime".into(),
            }],
        );
        let error = apply_to_specs(&[manifest], &mut specs).unwrap_err();
        assert!(error.contains("必需插件"), "{error}");
        assert!(specs[0].enabled);
    }

    #[test]
    fn manifest_cannot_override_a_builtin_id() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("manifest.json");
        std::fs::write(
            &file,
            serde_json::to_string(&sample(
                "news",
                vec![Step::DisablePlugin {
                    target: "version-manager".into(),
                }],
            ))
            .unwrap(),
        )
        .unwrap();
        let error = install(dir.path(), &file, true, &["news".to_string()]).unwrap_err();
        assert!(error.contains("内置插件"), "{error}");
    }

    #[test]
    fn unsigned_install_requires_explicit_flag() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("manifest.json");
        std::fs::write(
            &file,
            serde_json::to_string(&sample(
                "example-mirror",
                vec![Step::ProvideSource {
                    id: "campus".into(),
                    label: "校园镜像".into(),
                    priority: 20,
                    manifest_url: "https://mirror.example.edu/m.json".into(),
                    base_url: "https://mirror.example.edu".into(),
                }],
            ))
            .unwrap(),
        )
        .unwrap();

        assert!(install(dir.path(), &file, false, &[]).is_err());
        let (manifest, path) = install(dir.path(), &file, true, &[]).expect("显式允许后应当装上");
        assert_eq!(manifest.id, "example-mirror");
        assert!(path.is_file());
        assert_eq!(list(dir.path()).len(), 1);
        remove(dir.path(), "example-mirror").unwrap();
        assert!(list(dir.path()).is_empty());
    }

    #[test]
    fn sources_and_slots_are_collected_per_manifest() {
        let manifest = sample(
            "example",
            vec![
                Step::ProvideSource {
                    id: "campus".into(),
                    label: "校园镜像".into(),
                    priority: 20,
                    manifest_url: "https://mirror.example.edu/m.json".into(),
                    base_url: "https://mirror.example.edu".into(),
                },
                Step::BindSlot {
                    slot: "overview.notices".into(),
                    title: "说明".into(),
                    view: "readonly-text:校园网内优先走本地镜像".into(),
                },
                Step::SetDefaultSetting {
                    key: "checkBeforeLaunch".into(),
                    value: json!(true),
                },
            ],
        );
        let (owner, sources) = source_table(&manifest).expect("应当有下载源");
        assert_eq!(owner, "example");
        assert_eq!(sources[0].id(), "campus");
        assert_eq!(sources[0].priority(), 20);

        let slots = slots(&[manifest.clone()]);
        assert_eq!(slots[0]["slot"], "overview.notices");
        assert_eq!(slots[0]["text"], "校园网内优先走本地镜像");

        let defaults = default_settings(&[manifest]);
        assert_eq!(defaults[0].0, "checkBeforeLaunch");
    }
}
