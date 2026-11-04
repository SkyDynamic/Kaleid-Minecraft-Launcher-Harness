//! 补丁化组合：`presets/*.patch.yml`。
//!
//! 组合是"基线 + 补丁"：基线给出底座插件，补丁只描述差异（插入/改配置/停用/移除）。
//! 这样 `presets/minimal.patch.yml` 只需一行 `disable` 就能生成降级组合，
//! 而不必复制整份清单、随基线一起漂移。
//!
//! 补丁是**纯声明**的：条件只有 [`Predicate`](crate::plugin::Predicate) 那几种受限谓词，
//! 没有 `!!js`、没有脚本求值——补丁文件永远可以被静态审计。

use serde::Deserialize;
use serde_json::Value;

use crate::plugin::{PluginSpec, Predicate};

/// 一条补丁操作（程序化接口）。
#[derive(Debug, Clone)]
pub enum PatchOp {
    /// 追加若干插件规格。
    Insert(Vec<PluginSpec>),
    /// 修改已存在的规格。
    Update(UpdateOp),
    /// 停用（保留在组合里，便于插件管理页显示并允许用户重新启用）。
    Disable { id: String },
    /// 彻底移除。
    Remove { id: String },
}

/// 局部更新。
#[derive(Debug, Clone, Deserialize)]
pub struct UpdateOp {
    pub id: String,
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub config: Option<Value>,
    #[serde(default)]
    pub when: Option<Vec<Predicate>>,
}

/// YAML 里的一条补丁。YAML 没有 serde 的外部标记枚举语法，
/// 所以用「一个对象、四个可选键」表达，再校验恰好命中一个。
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawEntry {
    #[serde(default)]
    insert: Option<Vec<PluginSpec>>,
    #[serde(default)]
    update: Option<UpdateOp>,
    #[serde(default)]
    disable: Option<IdOnly>,
    #[serde(default)]
    remove: Option<IdOnly>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct IdOnly {
    id: String,
}

/// 补丁应用失败。
#[derive(Debug, thiserror::Error)]
pub enum PatchError {
    #[error("补丁 YAML 解析失败：{0}")]
    Yaml(String),
    #[error("一条补丁必须且只能指定 insert/update/disable/remove 之一")]
    AmbiguousEntry,
    #[error("补丁引用了不存在的插件 `{0}`")]
    UnknownTarget(String),
    #[error("插入的插件 `{0}` 已经存在")]
    Duplicate(String),
    #[error("插件 `{0}` 是必需插件，补丁不能停用它")]
    Required(String),
}

/// 解析补丁 YAML。
pub fn parse_patch(yaml: &str) -> Result<Vec<PatchOp>, PatchError> {
    let raw: Vec<RawEntry> =
        serde_yaml::from_str(yaml).map_err(|e| PatchError::Yaml(e.to_string()))?;
    let mut ops = Vec::with_capacity(raw.len());
    for entry in raw {
        let mut hits = 0;
        let mut op = None;
        if let Some(specs) = entry.insert {
            hits += 1;
            op = Some(PatchOp::Insert(specs));
        }
        if let Some(update) = entry.update {
            hits += 1;
            op = Some(PatchOp::Update(update));
        }
        if let Some(disable) = entry.disable {
            hits += 1;
            op = Some(PatchOp::Disable { id: disable.id });
        }
        if let Some(remove) = entry.remove {
            hits += 1;
            op = Some(PatchOp::Remove { id: remove.id });
        }
        match (hits, op) {
            (1, Some(op)) => ops.push(op),
            _ => return Err(PatchError::AmbiguousEntry),
        }
    }
    Ok(ops)
}

/// 在基线上依次应用补丁。
pub fn apply_patches(
    base: &[PluginSpec],
    patches: &[PatchOp],
) -> Result<Vec<PluginSpec>, PatchError> {
    let mut out: Vec<PluginSpec> = base.to_vec();
    for op in patches {
        match op {
            PatchOp::Insert(specs) => {
                for spec in specs {
                    if out.iter().any(|s| s.id == spec.id) {
                        return Err(PatchError::Duplicate(spec.id.clone()));
                    }
                    out.push(spec.clone());
                }
            }
            PatchOp::Update(update) => {
                let target = out
                    .iter_mut()
                    .find(|s| s.id == update.id)
                    .ok_or_else(|| PatchError::UnknownTarget(update.id.clone()))?;
                if let Some(enabled) = update.enabled {
                    if !enabled && target.required {
                        return Err(PatchError::Required(update.id.clone()));
                    }
                    target.enabled = enabled;
                }
                if let Some(cfg) = &update.config {
                    crate::context::merge_json(&mut target.config, cfg);
                }
                if let Some(when) = &update.when {
                    if target.required && !when.is_empty() {
                        return Err(PatchError::Required(update.id.clone()));
                    }
                    target.when = when.clone();
                }
            }
            PatchOp::Disable { id } => {
                let target = out
                    .iter_mut()
                    .find(|s| &s.id == id)
                    .ok_or_else(|| PatchError::UnknownTarget(id.clone()))?;
                if target.required {
                    return Err(PatchError::Required(id.clone()));
                }
                target.enabled = false;
            }
            PatchOp::Remove { id } => {
                let target = out
                    .iter()
                    .find(|s| &s.id == id)
                    .ok_or_else(|| PatchError::UnknownTarget(id.clone()))?;
                if target.required {
                    return Err(PatchError::Required(id.clone()));
                }
                out.retain(|s| &s.id != id);
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> Vec<PluginSpec> {
        vec![
            PluginSpec::new("core-lists"),
            PluginSpec::new("version-manager"),
            PluginSpec::new("launch"),
        ]
    }

    #[test]
    fn minimal_preset_disables_version_manager() {
        let patch = r#"
- disable:
    id: version-manager
"#;
        let ops = parse_patch(patch).unwrap();
        let result = apply_patches(&base(), &ops).unwrap();
        let vm = result.iter().find(|s| s.id == "version-manager").unwrap();
        assert!(!vm.enabled);
        assert!(result.iter().find(|s| s.id == "launch").unwrap().enabled);
    }

    #[test]
    fn patches_cannot_disable_required_plugins() {
        let base = vec![
            PluginSpec::new("settings").required(),
            PluginSpec::new("version-manager"),
        ];
        for yaml in [
            "- disable:\n    id: settings\n",
            "- update:\n    id: settings\n    enabled: false\n",
        ] {
            let ops = parse_patch(yaml).unwrap();
            let err = apply_patches(&base, &ops).unwrap_err();
            assert!(err.to_string().contains("必需插件"), "补丁也要拦：{err}");
        }
        // 只改配置、不在 enabled 上动手的 update 仍然放行。
        let ops = parse_patch("- update:\n    id: settings\n    config:\n      x: 1\n").unwrap();
        assert!(apply_patches(&base, &ops).is_ok());
    }

    #[test]
    fn patches_cannot_remove_required_plugins() {
        let base = vec![PluginSpec::new("settings").required()];
        let ops = parse_patch("- remove:\n    id: settings\n").unwrap();
        assert!(matches!(
            apply_patches(&base, &ops),
            Err(PatchError::Required(id)) if id == "settings"
        ));
        assert_eq!(base.len(), 1);
        assert!(base[0].enabled);
    }

    #[test]
    fn required_condition_updates_reject_even_currently_true_predicates() {
        let base = vec![PluginSpec::new("settings").required()];
        for platform in [std::env::consts::OS, "never-a-platform"] {
            let update = PatchOp::Update(UpdateOp {
                id: "settings".into(),
                enabled: None,
                config: None,
                when: Some(vec![Predicate::Platform {
                    platform: platform.into(),
                }]),
            });
            assert!(matches!(
                apply_patches(&base, &[update]),
                Err(PatchError::Required(id)) if id == "settings"
            ));
        }
        let ops =
            parse_patch("- update:\n    id: settings\n    when: []\n    config: {x: 1}\n").unwrap();
        let result = apply_patches(&base, &ops).unwrap();
        assert!(result[0].when.is_empty());
        assert_eq!(result[0].config["x"], 1);
    }

    #[test]
    fn insert_update_remove() {
        let patch = r#"
- insert:
    - id: modrinth
      name: modrinth
      enabled: true
      config:
        apiBase: "https://api.modrinth.com/v2"
- update:
    id: launch
    config:
      checkBeforeLaunch: false
- remove:
    id: core-lists
"#;
        let ops = parse_patch(patch).unwrap();
        let result = apply_patches(&base(), &ops).unwrap();
        assert!(result.iter().any(|s| s.id == "modrinth"));
        assert!(!result.iter().any(|s| s.id == "core-lists"));
        let launch = result.iter().find(|s| s.id == "launch").unwrap();
        assert_eq!(launch.config["checkBeforeLaunch"], serde_json::json!(false));
    }

    #[test]
    fn unknown_target_is_rejected() {
        let ops = parse_patch("- disable:\n    id: nope\n").unwrap();
        let err = apply_patches(&base(), &ops).unwrap_err();
        assert!(matches!(err, PatchError::UnknownTarget(_)));
    }

    #[test]
    fn duplicate_insert_is_rejected() {
        let ops = parse_patch("- insert:\n    - id: launch\n      name: launch\n").unwrap();
        let err = apply_patches(&base(), &ops).unwrap_err();
        assert!(matches!(err, PatchError::Duplicate(_)));
    }

    #[test]
    fn predicate_is_parsed_from_yaml() {
        let ops = parse_patch(
            r#"
- insert:
    - id: win-only
      name: win-only
      when:
        - platform: windows
        - env_set: KALEID_DEV
"#,
        )
        .unwrap();
        let result = apply_patches(&base(), &ops).unwrap();
        let spec = result.iter().find(|s| s.id == "win-only").unwrap();
        assert_eq!(spec.when.len(), 2);
        assert!(matches!(spec.when[0], Predicate::Platform { .. }));
        assert!(matches!(spec.when[1], Predicate::EnvSet { .. }));
    }

    #[test]
    fn ambiguous_entry_is_rejected() {
        let err = parse_patch("- disable:\n    id: a\n  remove:\n    id: b\n").unwrap_err();
        assert!(matches!(err, PatchError::AmbiguousEntry));
    }

    #[test]
    fn empty_entry_is_rejected() {
        let err = parse_patch("- {}\n").unwrap_err();
        assert!(matches!(err, PatchError::AmbiguousEntry));
    }
}
