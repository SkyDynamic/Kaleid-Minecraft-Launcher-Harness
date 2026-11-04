//! 分层设置（`SettingsServiceApi`）。
//!
//! 解析优先级：**版本 > 文件夹 > 全局 > 内置默认**。
//! 版本级设置刻意存在 Kaleid 自己的数据目录里，**不写进用户的 `.minecraft`**，
//! 避免污染游戏目录、也避免换文件夹后配置跟着错位。

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde_json::Value;

use kaleid_api::events::{GlobalSettingsChanged, VersionSettingsChanged};
use kaleid_api::{
    resolve_isolation, ApiError, ApiResult, EffectiveConfig, GlobalSettings, IsolationMode,
    SettingsServiceApi, VersionSettings,
};
use kaleid_kernel::Context;

use crate::jsonstore::{read_json, write_json};
use crate::paths;

pub struct Settings {
    data_root: PathBuf,
    ctx: Option<Context>,
    /// 用于解释"为什么是这个值"：记录上次解析出的全局默认，供 UI 参考。
    last_global: Mutex<Option<GlobalSettings>>,
}

impl Settings {
    pub fn open(data_root: &Path) -> Self {
        Settings {
            data_root: data_root.to_path_buf(),
            ctx: None,
            last_global: Mutex::new(None),
        }
    }

    pub fn with_context(mut self, ctx: Context) -> Self {
        self.ctx = Some(ctx);
        self
    }

    fn global_file(&self) -> PathBuf {
        paths::settings_file(&self.data_root)
    }

    fn version_file(&self, root_id: &str, version_id: &str) -> PathBuf {
        paths::version_settings_file(&self.data_root, root_id, version_id)
    }

    /// 读全局设置，顺手做一次性的迁移。
    ///
    /// 现在默认的 4 条 JVM 参数是**设置里的内容**（输入框里能看到、能改），
    /// 而老配置文件里它们还是"启动器暗中注入"的，存的是 `jvmArgs: []`。
    /// 直接把空数组交给界面，用户会看到一个空框——所以第一次读到时就补进去，
    /// 并记下 `jvmArgsInitialized`，从此 `[]` 的含义就是"用户自己删光了"。
    fn load_global(&self) -> ApiResult<GlobalSettings> {
        let value: Option<Value> = read_json(&self.global_file())?;
        match value {
            None => {
                let mut settings = GlobalSettings::default();
                settings.jvm_args_initialized = true;
                Ok(settings)
            }
            Some(v) => {
                let mut settings: GlobalSettings = serde_json::from_value(v)
                    .map_err(|e| ApiError::Format(format!("settings.json 结构不对：{e}")))?;
                // 老配置文件里存的是「自动 / 官方源 / BMCLAPI」和「自动 / 只用镜像 /
                // 只用官方」那两套取值，读盘时折算成新取值，免得界面选不中任何一项。
                settings.download_source =
                    kaleid_api::normalize_download_source(&settings.download_source);
                settings.mod_source = kaleid_api::normalize_mod_source(&settings.mod_source);
                settings.mod_description =
                    kaleid_api::normalize_mod_description(&settings.mod_description);
                if !settings.jvm_args_initialized {
                    settings.jvm_args_initialized = true;
                    if settings.jvm_args.is_empty() {
                        settings.jvm_args = kaleid_api::default_jvm_args();
                    }
                    // 迁移结果立刻落盘：不写回去的话每次读都要重算，
                    // 而且用户下一条 patch 会把它当成"新"值又抹一次。
                    write_json(&self.global_file(), &settings)?;
                }
                Ok(settings)
            }
        }
    }
}

impl SettingsServiceApi for Settings {
    fn global(&self) -> ApiResult<GlobalSettings> {
        self.load_global()
    }

    fn patch_global(&self, patch: &Value) -> ApiResult<GlobalSettings> {
        let current = self.load_global()?;
        let mut merged =
            serde_json::to_value(&current).map_err(|e| ApiError::Format(e.to_string()))?;
        kaleid_kernel::merge_json(&mut merged, patch);
        let mut next: GlobalSettings = serde_json::from_value(merged)
            .map_err(|e| ApiError::invalid(format!("设置项不合法：{e}")))?;

        if !(512..=32768).contains(&next.memory_mb) {
            return Err(ApiError::invalid("内存分配需要在 512–32768 MB 之间"));
        }
        if !(1..=256).contains(&next.download_concurrency) {
            return Err(ApiError::invalid("下载并发需要在 1–256 之间"));
        }
        if !kaleid_api::is_valid_download_source(&next.download_source) {
            return Err(ApiError::invalid(
                "下载源只能是 official-first / mirror-first / official-only / mirror-only",
            ));
        }
        if !kaleid_api::is_valid_mod_source(&next.mod_source) {
            return Err(ApiError::invalid(
                "Mod 数据源只能是 official-first / mirror-first / official-only",
            ));
        }
        if !kaleid_api::is_valid_mod_description(&next.mod_description) {
            return Err(ApiError::invalid(
                "Mod 列表注释只能是 original / translated",
            ));
        }
        // 游戏内标识：长度/字符集由契约层统一校验；非法值直接拒绝，
        // 而不是悄悄改掉用户输入（那会让「保存了但没生效」变得无从解释）。
        kaleid_api::validate_launch_brand(&next.launch_brand)?;
        if !kaleid_api::is_valid_launch_after_action(&next.launch_after_action) {
            return Err(ApiError::invalid(
                "启动游戏后操作只能是 keep / minimize / hide_to_tray / exit",
            ));
        }
        if !kaleid_api::is_valid_gc_mode(&next.gc_mode) {
            return Err(ApiError::invalid(
                "GC 策略只能是 zgc / zgc_gen / g1 / g1_tuned / none",
            ));
        }
        // JVM 参数逐行清理：空行丢掉，前后空白 trim。非法内容（换行里套引号那种）
        // 不在这里猜，交给 Java 自己报错——启动脚本里能看到原文。
        next.jvm_args = next
            .jvm_args
            .into_iter()
            .map(|a| a.trim().to_string())
            .filter(|a| !a.is_empty())
            .collect();

        write_json(&self.global_file(), &next)?;
        *self.last_global.lock().unwrap() = Some(next.clone());
        if let Some(ctx) = &self.ctx {
            ctx.emit(GlobalSettingsChanged {
                isolation_default: next.isolation_default,
                download_concurrency: next.download_concurrency,
            });
        }
        Ok(next)
    }

    fn version_settings(&self, root_id: &str, version_id: &str) -> ApiResult<VersionSettings> {
        Ok(
            read_json::<VersionSettings>(&self.version_file(root_id, version_id))?
                .unwrap_or_default(),
        )
    }

    fn patch_version(
        &self,
        root_id: &str,
        version_id: &str,
        patch: &Value,
    ) -> ApiResult<VersionSettings> {
        let current = self.version_settings(root_id, version_id)?;
        let mut merged =
            serde_json::to_value(&current).map_err(|e| ApiError::Format(e.to_string()))?;
        kaleid_kernel::merge_json(&mut merged, patch);
        let mut next: VersionSettings = serde_json::from_value(merged)
            .map_err(|e| ApiError::invalid(format!("设置项不合法：{e}")))?;

        if let Some(mb) = next.memory_mb {
            if !(512..=32768).contains(&mb) {
                return Err(ApiError::invalid("内存分配需要在 512–32768 MB 之间"));
            }
        }
        next.jvm_args = next
            .jvm_args
            .into_iter()
            .map(|a| a.trim().to_string())
            .filter(|a| !a.is_empty())
            .collect();
        // 老版本留下的文件里没有这两个键，反序列化出来是空串；
        // 先归一化再校验，否则「只改内存」这种补丁也会因为空模式被判非法。
        next.jvm_args_mode = kaleid_api::normalize_jvm_args_mode(&next.jvm_args_mode);
        if !kaleid_api::is_valid_jvm_args_mode(&next.jvm_args_mode) {
            return Err(ApiError::invalid("JVM 参数模式只能是 append / override"));
        }
        next.gc_mode = next
            .gc_mode
            .as_deref()
            .map(str::trim)
            .filter(|mode| !mode.is_empty())
            .map(str::to_string);
        if let Some(mode) = &next.gc_mode {
            if !kaleid_api::is_valid_gc_mode(mode) {
                return Err(ApiError::invalid(
                    "GC 策略只能是 zgc / zgc_gen / g1 / g1_tuned / none",
                ));
            }
        }
        if let Some(brand) = &next.launch_brand {
            kaleid_api::validate_launch_brand(brand)?;
        }

        write_json(&self.version_file(root_id, version_id), &next)?;

        if let Some(ctx) = &self.ctx {
            let folder = IsolationMode::Inherit;
            if let Ok(effective) = self.resolve(root_id, version_id, folder) {
                ctx.emit(VersionSettingsChanged {
                    root_id: root_id.to_string(),
                    version_id: version_id.to_string(),
                    effective,
                });
            }
        }
        Ok(next)
    }

    fn resolve(
        &self,
        root_id: &str,
        version_id: &str,
        folder_isolation: IsolationMode,
    ) -> ApiResult<EffectiveConfig> {
        let global = self.load_global()?;
        let version = self.version_settings(root_id, version_id)?;

        let isolated = resolve_isolation(folder_isolation, global.isolation_default);
        let isolated_from = match folder_isolation {
            IsolationMode::Inherit => "全局默认".to_string(),
            _ => "本文件夹设置".to_string(),
        };

        // 标识：显隐取最近一层写了显隐的，文本取最近一层写了非空文本的。
        let brand_enabled = version
            .launch_brand_enabled
            .unwrap_or(global.launch_brand_enabled);
        let brand_text = version
            .launch_brand
            .as_deref()
            .filter(|t| !t.trim().is_empty())
            .or(Some(global.launch_brand.as_str()));
        let launch_brand = kaleid_api::resolve_launch_brand(brand_enabled, brand_text);

        // JVM 参数：版本级可以选择"追加到全局后面"或"覆盖全局"。
        // 覆盖时全局参数整段不进命令行——这正是用户想要的语义：
        // 「这个版本的参数我自己管」。
        let jvm_args_mode =
            kaleid_api::normalize_jvm_args_mode(&version.jvm_args_mode);
        let mut jvm_args: Vec<String> = Vec::new();
        if jvm_args_mode == kaleid_api::JVM_ARGS_MODE_APPEND {
            jvm_args.extend(global.jvm_args.iter().cloned());
        }
        jvm_args.extend(version.jvm_args.iter().cloned());
        let gc_mode = version
            .gc_mode
            .clone()
            .unwrap_or_else(|| kaleid_api::normalize_gc_mode(&global.gc_mode));

        Ok(EffectiveConfig {
            memory_mb: version.memory_mb.unwrap_or(global.memory_mb),
            java_path: version.java_path.or_else(|| global.java_path.clone()),
            jvm_args,
            window: version.window.unwrap_or_else(|| "1280×720".to_string()),
            check_before_launch: version
                .check_before_launch
                .unwrap_or(global.check_before_launch),
            launch_brand,
            isolated,
            isolated_from,
            jvm_args_mode,
            gc_mode,
            // 开关默认是开的，但"高性能显卡"是 Windows 的注册表概念：
            // 在 Linux / macOS 上直接折算成关，免得界面显示已开启、实际什么也没做。
            high_performance_gpu: kaleid_api::effective_high_performance_gpu(
                global.high_performance_gpu,
            ),
            auto_chinese: global.auto_chinese,
            write_latest_launch_bat: global.write_latest_launch_bat,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn defaults_are_used_when_files_missing() {
        let dir = tempfile::tempdir().unwrap();
        let s = Settings::open(dir.path());
        let g = s.global().unwrap();
        assert_eq!(g.memory_mb, 4096);
        assert!(g.isolation_default);
        // 全新的数据目录：JVM 参数栏直接就是那四条默认值
        assert_eq!(g.jvm_args, kaleid_api::default_jvm_args());
        assert!(g.jvm_args_initialized);
        let e = s.resolve("r1", "1.21.4", IsolationMode::Inherit).unwrap();
        assert_eq!(e.memory_mb, 4096);
        assert!(e.isolated);
        assert_eq!(e.isolated_from, "全局默认");
        // 内存管理默认标准 G1GC；自动中文默认开
        assert_eq!(e.gc_mode, kaleid_api::GC_MODE_G1);
        assert!(e.auto_chinese);
        // 高性能显卡存的是开，但只有 Windows 上才算生效
        assert!(g.high_performance_gpu);
        assert_eq!(e.high_performance_gpu, cfg!(windows));
    }

    /// 老配置文件（那时默认参数由启动器暗中注入）里是 `jvmArgs: []`：
    /// 第一次读到要补成默认那四条并落盘，之后用户再清空就**不再**补。
    #[test]
    fn legacy_empty_jvm_args_are_seeded_once() {
        let dir = tempfile::tempdir().unwrap();
        let file = paths::settings_file(dir.path());
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, r#"{"memoryMb":4096,"jvmArgs":[]}"#).unwrap();

        let s = Settings::open(dir.path());
        let g = s.global().unwrap();
        assert_eq!(g.jvm_args, kaleid_api::default_jvm_args(), "老文件要补默认值");
        // 已经落盘：换个实例再读也不会重算
        let again = Settings::open(dir.path()).global().unwrap();
        assert_eq!(again.jvm_args, kaleid_api::default_jvm_args());

        // 用户清空 → 空就是空，重启后也不会被填回去
        s.patch_global(&json!({"jvmArgs": []})).unwrap();
        let cleared = Settings::open(dir.path()).global().unwrap();
        assert!(cleared.jvm_args.is_empty(), "用户删光了就该保持为空");
    }

    /// 老文件里如果已经写了自己的一两条参数，就不要动它，只打标记。
    #[test]
    fn legacy_custom_jvm_args_are_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let file = paths::settings_file(dir.path());
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, r#"{"jvmArgs":["-Dmine=1"]}"#).unwrap();

        let g = Settings::open(dir.path()).global().unwrap();
        assert_eq!(g.jvm_args, vec!["-Dmine=1".to_string()]);
        assert!(g.jvm_args_initialized);
    }

    #[test]
    fn version_overrides_global() {
        let dir = tempfile::tempdir().unwrap();
        let s = Settings::open(dir.path());
        s.patch_global(&json!({"memoryMb": 8192})).unwrap();
        s.patch_version("r1", "1.21.4", &json!({"memoryMb": 2048}))
            .unwrap();
        let e = s.resolve("r1", "1.21.4", IsolationMode::Inherit).unwrap();
        assert_eq!(e.memory_mb, 2048);
        // 别的版本仍取全局
        let e2 = s.resolve("r1", "1.20.1", IsolationMode::Inherit).unwrap();
        assert_eq!(e2.memory_mb, 8192);
    }

    #[test]
    fn folder_isolation_beats_global_default() {
        let dir = tempfile::tempdir().unwrap();
        let s = Settings::open(dir.path());
        s.patch_global(&json!({"isolationDefault": true})).unwrap();
        let e = s.resolve("r1", "v", IsolationMode::Off).unwrap();
        assert!(!e.isolated);
        assert_eq!(e.isolated_from, "本文件夹设置");

        s.patch_global(&json!({"isolationDefault": false})).unwrap();
        let e2 = s.resolve("r1", "v", IsolationMode::On).unwrap();
        assert!(e2.isolated);
    }

    #[test]
    fn invalid_values_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let s = Settings::open(dir.path());
        assert!(s.patch_global(&json!({"memoryMb": 1})).is_err());
        assert!(s.patch_global(&json!({"downloadConcurrency": 0})).is_err());
        assert!(s
            .patch_global(&json!({"downloadConcurrency": 257}))
            .is_err());
        assert_eq!(
            s.patch_global(&json!({"downloadConcurrency": 256}))
                .unwrap()
                .download_concurrency,
            256
        );
        assert!(s.patch_global(&json!({"downloadSource": "ftp"})).is_err());
        assert!(s.patch_global(&json!({"modSource": "ftp"})).is_err());
        assert!(s.patch_global(&json!({"modDescription": "ftp"})).is_err());
        assert_eq!(
            s.patch_global(&json!({"downloadSource": "mirror-first"}))
                .unwrap()
                .download_source,
            "mirror-first"
        );
        assert_eq!(
            s.patch_global(&json!({"modSource": "mirror-first"}))
                .unwrap()
                .mod_source,
            "mirror-first"
        );
        assert_eq!(
            s.patch_global(&json!({"modDescription": "translated"}))
                .unwrap()
                .mod_description,
            "translated"
        );
        assert!(s
            .patch_version("r1", "v", &json!({"memoryMb": 99999}))
            .is_err());
        // 非法写入不应落盘
        assert_eq!(s.global().unwrap().memory_mb, 4096);
    }

    #[test]
    fn legacy_source_values_are_upgraded_on_load() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path()).unwrap();
        // 老版本写下的配置：下载源 auto/mojang/bmclapi、Mod 数据源 auto/mcim/official。
        std::fs::write(
            dir.path().join("settings.json"),
            serde_json::to_string(&json!({
                "memoryMb": 2048,
                "downloadSource": "bmclapi",
                "modSource": "mcim"
            }))
            .unwrap(),
        )
        .unwrap();
        let s = Settings::open(dir.path());
        let g = s.global().unwrap();
        assert_eq!(g.download_source, "mirror-only");
        assert_eq!(g.mod_source, "mirror-first");
        // 缺字段（老文件没有 modDescription）也要落到「原文」。
        assert_eq!(g.mod_description, "original");
        assert_eq!(g.memory_mb, 2048);
        // 老文件里当然也没有「校园镜像兜底」这一项：默认开着，因为失败才生效。
        assert!(g.download_campus_mirrors);
    }

    /// 校园镜像兜底是个普通布尔设置项：关掉要能落盘，重开要能读回来。
    #[test]
    fn campus_mirrors_setting_roundtrips() {
        let dir = tempfile::tempdir().unwrap();
        let s = Settings::open(dir.path());
        assert!(s.global().unwrap().download_campus_mirrors, "缺省开着");
        let off = s
            .patch_global(&json!({"downloadCampusMirrors": false}))
            .unwrap();
        assert!(!off.download_campus_mirrors);
        // 重新打开一个实例，确认是落在 settings.json 里而不是只在内存里。
        let reopened = Settings::open(dir.path());
        assert!(!reopened.global().unwrap().download_campus_mirrors);
        let on = reopened
            .patch_global(&json!({"downloadCampusMirrors": true}))
            .unwrap();
        assert!(on.download_campus_mirrors);
    }

    #[test]
    fn jvm_args_are_trimmed_and_emptied() {
        let dir = tempfile::tempdir().unwrap();
        let s = Settings::open(dir.path());
        let v = s
            .patch_version("r1", "v", &json!({"jvmArgs": ["  -Xss1M ", "", "   "]}))
            .unwrap();
        assert_eq!(v.jvm_args, vec!["-Xss1M".to_string()]);
    }

    #[test]
    fn settings_not_written_into_game_dir() {
        // 版本设置必须落在 Kaleid 数据目录，而不是用户 .minecraft
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("kaleid");
        let mc = dir.path().join(".minecraft");
        std::fs::create_dir_all(&mc).unwrap();
        let s = Settings::open(&data);
        s.patch_version("r1", "1.21.4", &json!({"memoryMb": 2048}))
            .unwrap();
        assert!(data.join("version-settings/r1/1.21.4.json").is_file());
        let mut found = 0;
        for entry in std::fs::read_dir(&mc).unwrap() {
            let _ = entry.unwrap();
            found += 1;
        }
        assert_eq!(found, 0, "不应在游戏目录留下任何文件");
    }
}
