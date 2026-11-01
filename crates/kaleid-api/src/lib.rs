//! # kaleid-api
//!
//! 内核、插件与界面之间的公共语言：领域模型、事件载荷、服务契约、错误类型。
//! 这里的类型是**唯一**的真相来源，任何一侧都不允许再造平行结构。

pub mod admin;
pub mod content;
pub mod download;
pub mod error;
pub mod events;
pub mod java;
pub mod launch;
pub mod model;
pub mod mods;
pub mod msa;
pub mod services;

pub use admin::{DataPack, LoaderUpdateStatus, SaveDeleteOutcome, VersionDeleteOutcome};
pub use content::{
    ContentDirs, ContentKind, ContentServiceApi, ImportOutcome, ImportSkipped, LocalMod, LocalPack,
    LocalSave, LogFile, LogTail, ModIcon, ModUpdate, ModUpdateReport,
};
pub use download::{
    DownloadJob, DownloadPhase, DownloadSnapshot, DownloadStatus, FabricApiInfo, InstallLoader,
    InstallQueued, InstallRequest, LoaderVersion, LoaderVersionTimes, LoaderVersions, OptifineRef,
    PhaseStatus, RemoteVersion, RemoteVersions, VersionNameCheck,
};
pub use error::{ApiError, ApiResult};
pub use java::{JavaRuntime, JavaServiceApi, JavaSource};
pub use launch::{
    fresh_launch_steps, launch_progress, CrashArchive, CrashExport, CrashKeyword, CrashReport,
    CrashSuspect, LaunchBlock, LaunchBlockKind, LaunchCheck, LaunchPhase, LaunchState, LaunchStep,
    SuspectConfidence, LAUNCH_STEPS, LAUNCH_STEP_WEIGHTS,
};
pub use model::{
    default_jvm_args, effective_high_performance_gpu, is_valid_download_source, is_valid_gc_mode,
    is_valid_jvm_args_mode, is_valid_launch_after_action, normalize_download_source,
    normalize_gc_mode, normalize_jvm_args_mode, normalize_launch_after_action, offline_uuid,
    resolve_isolation, resolve_launch_brand, validate_launch_brand, validate_offline_name, Account,
    AccountKind, EffectiveConfig, GlobalSettings, IntegrityReport, IsolationMode, LaunchPlan,
    LoaderKind, NativeJarRef, Pick, Readiness, Root, RootKind, VersionSettings, VersionSummary,
    DEFAULT_JVM_ARGS, DOWNLOAD_SOURCE_AUTO, DOWNLOAD_SOURCE_MIRROR_FIRST,
    DOWNLOAD_SOURCE_MIRROR_ONLY, DOWNLOAD_SOURCE_OFFICIAL_FIRST, DOWNLOAD_SOURCE_OFFICIAL_ONLY,
    GC_MODE_G1, GC_MODE_G1_TUNED, GC_MODE_NONE, GC_MODE_ZGC, GC_MODE_ZGC_GENERATIONAL,
    HIGH_PERFORMANCE_GPU_SUPPORTED,
    JVM_ARGS_MODE_APPEND, JVM_ARGS_MODE_OVERRIDE, LAUNCH_AFTER_EXIT, LAUNCH_AFTER_HIDE_TO_TRAY,
    LAUNCH_AFTER_KEEP, LAUNCH_AFTER_MINIMIZE, LAUNCH_BRAND_DEFAULT, LAUNCH_BRAND_MAX_CHARS,
    OFFLINE_NAME_PATTERN,
};
pub use mods::{
    format_downloads, format_game_range, game_family, is_valid_mod_description,
    is_valid_mod_source, join_loader_labels, loader_label, mirror_url_for, normalize_loader,
    normalize_mod_description, normalize_mod_source, normalize_name, sorted_game_families,
    InstalledMod, ModDetail, ModDetailQuery, ModFile, ModHit, ModProject, ModSearchQuery,
    ModSearchResult, ModSource, ModSourceFilter, ModTarget, ModType, ModVersionGroup,
    MOD_DESCRIPTION_ORIGINAL, MOD_DESCRIPTION_TRANSLATED, MOD_SOURCE_MIRROR_FIRST,
    MOD_SOURCE_OFFICIAL_FIRST, MOD_SOURCE_OFFICIAL_ONLY,
};
pub use msa::{DeviceCodeInfo, MsaAccountState, MsaAuthApi, MsaConfig, MsaPoll};
pub use services::{
    AccountServiceApi, InstallSourceApi, LaunchPlannerApi, RootRegistryService, SettingsServiceApi,
    VersionStoreService,
};
