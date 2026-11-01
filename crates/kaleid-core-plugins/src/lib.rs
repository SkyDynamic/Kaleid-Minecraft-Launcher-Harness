//! Kaleid 内置插件集合。
//!
//! | 模块 | 提供的能力 |
//! |---|---|
//! | [`roots`] | `RootRegistryService`：四种文件夹收录情形、Roaming 自动收录、别名、隔离三态 |
//! | [`versions`] | `VersionStoreService`：扫描、分类（原版/Fabric/NeoForge/未识别）、就绪度 |
//! | [`settings`] | `SettingsServiceApi`：版本 > 文件夹 > 全局 > 内置默认 的分层解析 |
//! | [`accounts`] | `AccountServiceApi`：全局账号、多个正版、离线**输入即生效** |
//! | [`launch`] | `LaunchPlannerApi`：`inheritsFrom` 合并、rules 过滤、变量替换、隔离 gameDir |
//! | [`plugins`] | 上述能力的插件外壳、下载源、组合 preset |

pub mod accounts;
pub mod admin;
pub mod content;
pub mod crash;
pub mod crash_zip;
pub mod downloads;
pub mod fingerprint;
pub mod game;
pub mod gpu;
pub mod hostlimit;
pub mod install;
pub mod jar;
pub mod java;
pub mod jsonstore;
pub mod jvm;
pub mod launch;
pub mod loaders;
pub mod manifests;
pub mod modnames;
pub mod mods;
pub mod msa;
pub mod natives;
pub mod nbt;
pub mod options;
pub mod paths;
pub mod plugins;
pub mod remote;
pub mod repair;
pub mod roots;
pub mod saves;
pub mod settings;
pub mod skin;
pub mod urlcache;
pub mod versions;

pub use accounts::Accounts;
pub use admin::VersionAdmin;
pub use content::{resolve_game_dir, ContentService, LOG_TAIL_DEFAULT, LOG_TAIL_MAX};
pub use crash::analyze as analyze_crash;
pub use downloads::DownloadManager;
pub use fingerprint::{curseforge_fingerprint, fingerprint_file};
pub use game::GameProcessManager;
pub use hostlimit::{HostLimits, HostState};
pub use install::InstallService;
pub use jar::{list_entries, read_entry_bytes, read_mod_meta, ModMeta};
pub use java::{JavaService, ScanEnv};
pub use launch::LaunchPlanner;
pub use manifests::{
    apply_to_specs, default_settings, install as install_manifest, list as list_manifests,
    load_ready as load_ready_manifests, plugins_dir, remove as remove_manifest,
    slots as manifest_slots, source_table as manifest_source_table, InstalledManifest, Manifest,
    Step as ManifestStep, MANIFEST_API,
};
pub use loaders::{
    fabric_api_latest, fabric_loaders, fabric_profile, forge_installer_sha1_url,
    forge_installer_url, forge_loaders, loader_times, neoforge_installer_url, neoforge_loaders,
    neoforge_prefix,
};
pub use modnames::{ModNames, NameEntry};
pub use mods::{mod_types, ModInstallRequest, ModRankingQuery, ModService, USER_AGENT};
pub use msa::{FileTokenStore, MemoryTokenStore, MsaAuth, TokenStore};
pub use paths::data_root;
pub use plugins::{
    all_install_sources, builtin_sources, describe_plugins, minimal_preset, register_all, roots_of,
    selected_root, settings_of, standard_preset, InstallSourceTable, PluginDescriptor,
};
pub use remote::{parse_manifest, RemoteClient};
pub use roots::RootRegistry;
pub use saves::{SaveManager, SaveSummary};
pub use settings::Settings;
pub use urlcache::{Resolved, UrlCache};
pub use versions::VersionStore;
