//! 文件夹收录（`RootRegistryService`）。
//!
//! 关键行为：
//! - 首次启动自动收录 `%APPDATA%\.minecraft`，别名「默认（Roaming）」，`auto = true`；
//!   用户把它移出列表后**不会**在下次启动时复活。
//! - 移除只取消注册，**从不删磁盘**。
//! - 空目录可以收录，`.minecraft` 等到第一次装版本时才创建。

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use kaleid_api::events::RootsChanged;
use kaleid_api::{ApiError, ApiResult, IsolationMode, Pick, Root, RootKind, RootRegistryService};
use kaleid_kernel::Context;

use crate::jsonstore::{now_iso, read_json, write_json};
use crate::paths;

#[derive(Debug, Default, Serialize, Deserialize)]
struct PersistedRoots {
    #[serde(default)]
    roots: Vec<Root>,
    #[serde(default)]
    selected: Option<String>,
    /// 是否已经做过"首次启动自动收录"。与 `roots` 分开存，这样用户删掉 Roaming 后
    /// 它不会因为"列表为空"被重新加回来。
    #[serde(default)]
    auto_seeded: bool,
    /// 默认目录：列表里置顶，下次打开启动器优先选中。
    ///
    /// 与 `selected` 分开：`selected` 是"这次在用哪个"，`default_id` 是"用户希望
    /// 每次打开时默认是哪个"。两者相同是常态，但切换目录不该顺手改掉默认。
    #[serde(default)]
    default_id: Option<String>,
}

pub struct RootRegistry {
    file: PathBuf,
    state: Mutex<PersistedRoots>,
    ctx: Option<Context>,
}

impl RootRegistry {
    /// 打开（或初始化）注册表。
    pub fn open(data_root: &Path, ctx: Option<Context>) -> ApiResult<Self> {
        let file = paths::roots_file(data_root);
        let mut persisted: PersistedRoots = read_json(&file)?.unwrap_or_default();
        let mut changed = false;

        if !persisted.auto_seeded {
            persisted.auto_seeded = true;
            changed = true;
            if let Some(roaming) = roaming_minecraft() {
                if roaming.is_dir()
                    && !persisted
                        .roots
                        .iter()
                        .any(|r| same_path(&r.path, &roaming.to_string_lossy()))
                {
                    persisted.roots.push(Root {
                        id: new_id(),
                        path: roaming.to_string_lossy().to_string(),
                        kind: RootKind::BareMinecraft,
                        alias: "默认（Roaming）".to_string(),
                        added_at: now_iso(),
                        isolation: IsolationMode::Inherit,
                        auto: true,
                        last_used: None,
                    });
                }
            }
        }

        // 选中项失效时回落到第一个
        if let Some(sel) = persisted.selected.clone() {
            if !persisted.roots.iter().any(|r| r.id == sel) {
                persisted.selected = persisted.roots.first().map(|r| r.id.clone());
                changed = true;
            }
        } else if !persisted.roots.is_empty() {
            persisted.selected = persisted.roots.first().map(|r| r.id.clone());
            changed = true;
        }

        let registry = RootRegistry {
            file,
            state: Mutex::new(persisted),
            ctx,
        };
        if changed {
            registry.persist()?;
        }
        Ok(registry)
    }

    /// 只读构造（测试用）：不自动收录 Roaming。
    pub fn empty_at(data_root: &Path) -> ApiResult<Self> {
        let file = paths::roots_file(data_root);
        Ok(RootRegistry {
            file,
            state: Mutex::new(PersistedRoots {
                auto_seeded: true,
                ..Default::default()
            }),
            ctx: None,
        })
    }

    pub fn with_context(mut self, ctx: Context) -> Self {
        self.ctx = Some(ctx);
        self
    }

    fn persist(&self) -> ApiResult<()> {
        let snapshot = {
            let g = self.state.lock().unwrap();
            PersistedRoots {
                roots: g.roots.clone(),
                selected: g.selected.clone(),
                auto_seeded: g.auto_seeded,
                default_id: g.default_id.clone(),
            }
        };
        write_json(&self.file, &snapshot)
    }

    fn emit(&self) {
        if let Some(ctx) = &self.ctx {
            let (roots, selected) = {
                let g = self.state.lock().unwrap();
                (g.roots.clone(), g.selected.clone())
            };
            ctx.emit(RootsChanged { roots, selected });
        }
    }

    /// 解析某个文件夹当前是否启用版本隔离所需的原始三态。
    pub fn isolation_of(&self, id: &str) -> ApiResult<IsolationMode> {
        let g = self.state.lock().unwrap();
        g.roots
            .iter()
            .find(|r| r.id == id)
            .map(|r| r.isolation)
            .ok_or_else(|| ApiError::not_found(format!("文件夹 {id}")))
    }
}

fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

fn same_path(a: &str, b: &str) -> bool {
    let norm = |s: &str| {
        s.replace('/', "\\")
            .trim_end_matches('\\')
            .to_ascii_lowercase()
    };
    norm(a) == norm(b)
}

fn roaming_minecraft() -> Option<PathBuf> {
    if let Ok(custom) = std::env::var("KALEID_APPDATA") {
        if !custom.is_empty() {
            return Some(PathBuf::from(custom).join(".minecraft"));
        }
    }
    std::env::var("APPDATA")
        .ok()
        .filter(|v| !v.is_empty())
        .map(|v| PathBuf::from(v).join(".minecraft"))
}

/// 从一个路径推导默认别名。
fn default_alias(path: &Path, kind: RootKind) -> String {
    match kind {
        RootKind::BareMinecraft => {
            if path
                .file_name()
                .map(|n| n.eq_ignore_ascii_case(".minecraft"))
                .unwrap_or(false)
            {
                path.parent()
                    .and_then(|p| p.file_name())
                    .map(|n| format!("{}（.minecraft）", n.to_string_lossy()))
                    .unwrap_or_else(|| ".minecraft".to_string())
            } else {
                path.file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| "未命名文件夹".to_string())
            }
        }
        RootKind::ParentWithMc => path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "未命名文件夹".to_string()),
        RootKind::EmptyFolder => {
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "新目录".to_string());
            format!("{name}（待创建）")
        }
        RootKind::OrdinaryDir => path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "未命名文件夹".to_string()),
    }
}

impl RootRegistryService for RootRegistry {
    fn list(&self) -> ApiResult<Vec<Root>> {
        let g = self.state.lock().unwrap();
        let mut roots = g.roots.clone();
        // 默认目录置顶：界面直接按这个顺序渲染，不用再自己排一次。
        if let Some(default_id) = g.default_id.as_ref() {
            if let Some(idx) = roots.iter().position(|r| &r.id == default_id) {
                let pinned = roots.remove(idx);
                roots.insert(0, pinned);
            }
        }
        Ok(roots)
    }

    fn get(&self, id: &str) -> ApiResult<Root> {
        let g = self.state.lock().unwrap();
        g.roots
            .iter()
            .find(|r| r.id == id)
            .cloned()
            .ok_or_else(|| ApiError::not_found(format!("文件夹 {id}")))
    }

    fn add(&self, path: &Path, alias: Option<&str>, force: bool) -> ApiResult<Root> {
        if path.as_os_str().is_empty() {
            return Err(ApiError::invalid("没有选择文件夹"));
        }
        let pick = Pick::classify(path);
        if matches!(pick, Pick::OrdinaryDir { .. }) && !force {
            return Err(ApiError::invalid(
                "这个目录既不是 .minecraft，里面也没有 .minecraft 子目录。确认后可以照常收录，\
                 但只有在其中创建 .minecraft 之后才会有版本。",
            ));
        }
        let root_path = pick
            .root_path()
            .ok_or_else(|| ApiError::invalid("无法解析该路径"))?
            .to_string();
        let kind = pick.kind();

        let mut g = self.state.lock().unwrap();
        if g.roots.iter().any(|r| same_path(&r.path, &root_path)) {
            return Err(ApiError::invalid("这个文件夹已经在列表里了"));
        }

        let root = Root {
            id: new_id(),
            path: root_path.clone(),
            kind,
            alias: alias
                .map(|a| a.trim().to_string())
                .filter(|a| !a.is_empty())
                .unwrap_or_else(|| default_alias(Path::new(&root_path), kind)),
            added_at: now_iso(),
            isolation: IsolationMode::Inherit,
            auto: false,
            last_used: None,
        };
        g.roots.push(root.clone());
        if g.selected.is_none() {
            g.selected = Some(root.id.clone());
        }
        drop(g);

        self.persist()?;
        self.emit();
        Ok(root)
    }

    fn remove(&self, id: &str) -> ApiResult<()> {
        {
            let mut g = self.state.lock().unwrap();
            let before = g.roots.len();
            g.roots.retain(|r| r.id != id);
            if g.roots.len() == before {
                return Err(ApiError::not_found(format!("文件夹 {id}")));
            }
            if g.selected.as_deref() == Some(id) {
                g.selected = g.roots.first().map(|r| r.id.clone());
            }
            // 默认目录被移除后不能指向不存在的条目，否则 `list()` 每次都要多判一次。
            if g.default_id.as_deref() == Some(id) {
                g.default_id = None;
            }
        }
        self.persist()?;
        self.emit();
        Ok(())
    }

    fn rename(&self, id: &str, alias: &str) -> ApiResult<Root> {
        let trimmed = alias.trim();
        if trimmed.is_empty() {
            return Err(ApiError::invalid("别名不能为空"));
        }
        if trimmed.chars().count() > 32 {
            return Err(ApiError::invalid("别名最多 32 个字符"));
        }
        let updated = {
            let mut g = self.state.lock().unwrap();
            let root = g
                .roots
                .iter_mut()
                .find(|r| r.id == id)
                .ok_or_else(|| ApiError::not_found(format!("文件夹 {id}")))?;
            root.alias = trimmed.to_string();
            // 用户改过别名就不再是"自动收录的原始条目"
            root.auto = false;
            root.clone()
        };
        self.persist()?;
        self.emit();
        Ok(updated)
    }

    fn set_isolation(&self, id: &str, mode: IsolationMode) -> ApiResult<Root> {
        let updated = {
            let mut g = self.state.lock().unwrap();
            let root = g
                .roots
                .iter_mut()
                .find(|r| r.id == id)
                .ok_or_else(|| ApiError::not_found(format!("文件夹 {id}")))?;
            root.isolation = mode;
            root.clone()
        };
        self.persist()?;
        self.emit();
        Ok(updated)
    }

    fn select(&self, id: &str) -> ApiResult<Root> {
        let updated = {
            let mut g = self.state.lock().unwrap();
            let root = g
                .roots
                .iter_mut()
                .find(|r| r.id == id)
                .ok_or_else(|| ApiError::not_found(format!("文件夹 {id}")))?;
            root.last_used = Some(now_iso());
            let snapshot = root.clone();
            g.selected = Some(id.to_string());
            snapshot
        };
        self.persist()?;
        self.emit();
        Ok(updated)
    }

    fn selected(&self) -> ApiResult<Option<Root>> {
        let g = self.state.lock().unwrap();
        Ok(g.selected
            .as_ref()
            .and_then(|id| g.roots.iter().find(|r| &r.id == id))
            .cloned())
    }

    fn set_default(&self, id: &str) -> ApiResult<Root> {
        let updated = {
            let mut g = self.state.lock().unwrap();
            let root = g
                .roots
                .iter()
                .find(|r| r.id == id)
                .cloned()
                .ok_or_else(|| ApiError::not_found(format!("文件夹 {id}")))?;
            g.default_id = Some(id.to_string());
            root
        };
        self.persist()?;
        self.emit();
        Ok(updated)
    }

    fn default_root(&self) -> ApiResult<Option<Root>> {
        let g = self.state.lock().unwrap();
        Ok(g.default_id
            .as_ref()
            .and_then(|id| g.roots.iter().find(|r| &r.id == id))
            .cloned())
    }

    fn clear_default(&self) -> ApiResult<()> {
        {
            let mut g = self.state.lock().unwrap();
            g.default_id = None;
        }
        self.persist()?;
        self.emit();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn add_bare_minecraft() {
        let dir = tmp();
        let mc = dir.path().join(".minecraft");
        std::fs::create_dir_all(mc.join("versions")).unwrap();
        let reg = RootRegistry::empty_at(&dir.path().join("data")).unwrap();
        let root = reg.add(&mc, None, false).unwrap();
        assert_eq!(root.kind, RootKind::BareMinecraft);
        assert_eq!(root.isolation, IsolationMode::Inherit);
        assert!(root.alias.contains(".minecraft"));
        assert_eq!(reg.list().unwrap().len(), 1);
    }

    #[test]
    fn add_parent_with_minecraft_uses_nested_root() {
        let dir = tmp();
        let parent = dir.path().join("Exp");
        std::fs::create_dir_all(parent.join(".minecraft")).unwrap();
        let reg = RootRegistry::empty_at(&dir.path().join("data")).unwrap();
        let root = reg.add(&parent, Some("存档实验场"), false).unwrap();
        assert_eq!(root.kind, RootKind::ParentWithMc);
        assert_eq!(root.alias, "存档实验场");
        assert!(root.path.ends_with(".minecraft"));
    }

    #[test]
    fn add_empty_folder_is_accepted_without_creating_minecraft() {
        let dir = tmp();
        let empty = dir.path().join("mc-new");
        std::fs::create_dir_all(&empty).unwrap();
        let reg = RootRegistry::empty_at(&dir.path().join("data")).unwrap();
        let root = reg.add(&empty, None, false).unwrap();
        assert_eq!(root.kind, RootKind::EmptyFolder);
        // 收录时不创建 .minecraft
        assert!(!empty.join(".minecraft").exists());
    }

    #[test]
    fn ordinary_dir_requires_confirmation() {
        let dir = tmp();
        let ordinary = dir.path().join("docs");
        std::fs::create_dir_all(&ordinary).unwrap();
        std::fs::write(ordinary.join("a.txt"), b"x").unwrap();
        let reg = RootRegistry::empty_at(&dir.path().join("data")).unwrap();
        let err = reg.add(&ordinary, None, false).unwrap_err();
        assert!(matches!(err, ApiError::Invalid(_)));
        let ok = reg.add(&ordinary, None, true).unwrap();
        assert_eq!(ok.kind, RootKind::OrdinaryDir);
    }

    #[test]
    fn duplicate_path_is_rejected_case_insensitively() {
        let dir = tmp();
        let mc = dir.path().join(".minecraft");
        std::fs::create_dir_all(&mc).unwrap();
        let reg = RootRegistry::empty_at(&dir.path().join("data")).unwrap();
        reg.add(&mc, None, false).unwrap();
        let alt = dir.path().join(".MINECRAFT");
        let err = reg.add(&alt, None, false).unwrap_err();
        assert!(matches!(err, ApiError::Invalid(_)));
    }

    #[test]
    fn remove_only_unregisters_and_falls_back_selection() {
        let dir = tmp();
        let a = dir.path().join("a/.minecraft");
        let b = dir.path().join("b/.minecraft");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        let reg = RootRegistry::empty_at(&dir.path().join("data")).unwrap();
        let ra = reg.add(&a, None, false).unwrap();
        let rb = reg.add(&b, None, false).unwrap();
        reg.select(&ra.id).unwrap();
        reg.remove(&ra.id).unwrap();
        // 磁盘没被动
        assert!(a.is_dir());
        assert_eq!(reg.selected().unwrap().unwrap().id, rb.id);
    }

    #[test]
    fn rename_validates() {
        let dir = tmp();
        let mc = dir.path().join(".minecraft");
        std::fs::create_dir_all(&mc).unwrap();
        let reg = RootRegistry::empty_at(&dir.path().join("data")).unwrap();
        let r = reg.add(&mc, None, false).unwrap();
        assert!(reg.rename(&r.id, "   ").is_err());
        let renamed = reg.rename(&r.id, "  主力  ").unwrap();
        assert_eq!(renamed.alias, "主力");
        assert!(!renamed.auto);
    }

    #[test]
    fn roaming_is_seeded_once_and_never_resurrected() {
        let dir = tmp();
        let appdata = dir.path().join("appdata");
        std::fs::create_dir_all(appdata.join(".minecraft")).unwrap();
        std::env::set_var("KALEID_APPDATA", &appdata);
        let data = dir.path().join("data");

        let reg = RootRegistry::open(&data, None).unwrap();
        let roots = reg.list().unwrap();
        assert_eq!(roots.len(), 1);
        assert_eq!(roots[0].alias, "默认（Roaming）");
        assert!(roots[0].auto);

        // 用户移除后重启，不应复活
        reg.remove(&roots[0].id).unwrap();
        drop(reg);
        let reg2 = RootRegistry::open(&data, None).unwrap();
        assert!(reg2.list().unwrap().is_empty());
        std::env::remove_var("KALEID_APPDATA");
    }
}
