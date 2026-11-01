/**
 * Commands and payload types for the Kaleid host <-> UI contract.
 * Every key is camelCase and mirrors the Rust side one-to-one.
 */

export type IsolationMode = 'inherit' | 'on' | 'off';
export type RootKind = 'bare_minecraft' | 'parent_with_mc' | 'empty_folder' | 'ordinary_dir';

export interface Root {
  id: string;
  path: string;
  kind: RootKind;
  alias: string;
  addedAt: string;
  isolation: IsolationMode;
  auto: boolean;
  lastUsed: string | null;
}

export type LoaderKind = 'vanilla' | 'fabric' | 'forge' | 'neoforge' | 'optifine' | 'unrecognized';

export interface RemoteVersion {
  id: string;
  /** Mojang 原始类型：release / snapshot / old_beta / old_alpha。 */
  type: string;
  releaseTime: string;
  url: string;
  /** release / snapshot / ancient / april_fools */
  category: 'release' | 'snapshot' | 'ancient' | 'april_fools';
  aprilFoolsYear: number | null;
  aprilFoolsIntro: string | null;
}

export interface RemoteVersions {
  latestRelease: string | null;
  latestSnapshot: string | null;
  versions: RemoteVersion[];
  fetchedAt: number;
}

export interface LoaderVersion {
  version: string;
  stable: boolean;
  recommended: boolean;
  time: string | null;
}

export interface LoaderVersions {
  gameVersion: string;
  loader: 'fabric' | 'forge' | 'neoforge' | string;
  versions: LoaderVersion[];
  recommended: string | null;
  fetchedAt: number;
}

export interface LoaderVersionTimes {
  times: Record<string, string | null>;
}

export interface FabricApiInfo {
  version: string;
  fileName: string;
  downloadUrl: string;
  size: number;
  sha1: string | null;
}

export interface VersionNameCheck {
  ok: boolean;
  error: string | null;
}

export type InstallLoader =
  | { kind: 'vanilla' }
  | { kind: 'fabric'; version: string }
  | { kind: 'forge'; version: string }
  | { kind: 'neoforge'; version: string };

/**
 * OptiFine 的版本指认：`kind` 是 `HD_U`，`patch` 是 `J3`，拼起来就是 `HD_U_J3`。
 * 它是「加载器之外再加一层」的东西：高版本要挂在 Forge 上，低版本可以单装。
 */
export interface OptifineRef {
  kind: string;
  patch: string;
}

export interface InstallRequest {
  rootId: string;
  gameVersion: string;
  versionName: string;
  loader: InstallLoader;
  installFabricApi: boolean;
  optifine?: OptifineRef | null;
}

export interface InstallQueued {
  jobId: string;
}

export type DownloadStatus =
  | 'resolving'
  | 'queued'
  | 'running'
  | 'completed'
  | 'failed'
  | 'cancelled';

export type PhaseStatus = 'pending' | 'running' | 'completed' | 'skipped' | 'failed';

export interface DownloadPhase {
  id: 'prepare' | 'client' | 'libraries' | 'assets' | 'loader' | 'fabric-api' | 'installer' | string;
  label: string;
  status: PhaseStatus;
  doneBytes: number;
  totalBytes: number;
  filesDone: number;
  filesTotal: number;
}

export interface DownloadJob {
  id: string;
  title: string;
  versionName: string;
  rootId: string;
  gameVersion: string;
  loaderKind: LoaderKind;
  loaderVersion: string | null;
  source: string;
  status: DownloadStatus;
  current: string;
  doneBytes: number;
  totalBytes: number;
  filesDone: number;
  filesTotal: number;
  percent: number;
  error: string | null;
  phases: DownloadPhase[];
  createdAt: number;
}

export interface DownloadSnapshot {
  jobs: DownloadJob[];
  doneBytes: number;
  totalBytes: number;
  percent: number;
  speedBps: number;
  concurrency: number;
  activeWorkers: number;
  activeJobs: number;
  queuedJobs: number;
}
export type Readiness = 'ready' | 'unknown' | { incomplete: { missing: number } };

export interface VersionSummary {
  id: string;
  loader: LoaderKind;
  loaderVersion: string | null;
  inheritsFrom: string | null;
  /** 基础原版版本号（自包含安装时来自 `kaleidBaseGameVersion`）。 */
  gameVersion: string | null;
  readiness: Readiness;
  javaMajor: number | null;
  jsonPath: string;
  lastModified: string | null;
}

export type AccountKind = 'msa' | 'offline';

export interface Account {
  id: string;
  kind: AccountKind;
  name: string;
  uuid: string;
  skinModel: string;
  /**
   * 皮肤贴图直链（`textures.minecraft.net/texture/<hash>`）。
   *
   * 账号记录里通常为 `null`：`McHead` 会用 `name` / `uuid` 调宿主
   * `skin_resolve`（官方 sessionserver → mc-heads → 内置 Steve/Alex）。
   * 如果账号插件以后直接带回直链，也可以在这里预填。
   */
  skinUrl: string | null;
  lastUsed: string | null;
}

/* ---------------------------------------------------------------- *
 * 新闻（只读展示数据）
 *
 * 来源：官方 Mojang 启动器新闻源 `launchercontent.mojang.com/v2/news.json`。
 * 这是界面直接读的公开只读 feed，不经过宿主命令——宿主目前没有任何 HTTP
 * 依赖，为一条新闻引入 TLS 栈不值得（详见设计规格「已知取舍」）。
 * ---------------------------------------------------------------- */

export interface NewsEntry {
  id: string;
  title: string;
  /** 原文分类，例如 `Minecraft: Java Edition`。 */
  category: string;
  /** `YYYY-MM-DD`。 */
  date: string;
  text: string;
  /** 已拼成绝对地址的配图；缺图时为 `null`。 */
  imageUrl: string | null;
  /** 原文链接（点击用系统浏览器打开）。 */
  link: string;
}

export interface GlobalSettings {
  memoryMb: number;
  isolationDefault: boolean;
  language: string;
  theme: string;
  downloadConcurrency: number;
  /** 下载源：auto（自动，推荐）/ official-first / mirror-first / official-only / mirror-only。 */
  downloadSource: string;
  /** Mod 数据源：official-first（官方优先）/ mirror-first（镜像优先）/ official-only（仅官方）。 */
  modSource: string;
  /** Mod 列表注释：original（原文）/ translated（MCIM 译文，不受数据源影响）。 */
  modDescription: string;
  checkBeforeLaunch: boolean;
  javaPath: string | null;
  /** 是否在游戏里显示启动器标识（`--versionType`）。 */
  launchBrandEnabled: boolean;
  /** 自定义标识文本；空白等价于隐藏。 */
  launchBrand: string;
  /** 启动游戏后启动器自己做什么。 */
  launchAfterAction: LaunchAfterAction;
  /** 全局 JVM 参数（一条一项）。默认就是推荐的四条，可改可删。版本级可以追加或覆盖它。 */
  jvmArgs: string[];
  /**
   * 「JVM 参数」这一栏是否已经补过默认值。
   *
   * 老配置文件里存的是 `jvmArgs: []`（那时默认参数由启动器暗中注入），读盘时补一次
   * 并置上这个标记；此后空数组就表示「用户自己删光了」，界面不该再填回去。
   */
  jvmArgsInitialized: boolean;
  /** GC 档位；Java 版本不够时启动器会自动降级并在启动状态里说明。 */
  gcMode: GcMode;
  /** 把启动器与 Java 都设成「使用高性能显卡」（仅 Windows，写注册表）。 */
  highPerformanceGpu: boolean;
  /** 启动前把游戏语言调成简体中文。 */
  autoChinese: boolean;
  /** 每次启动都写一份 `<数据目录>/LatestLaunch.bat`。 */
  writeLatestLaunchBat: boolean;
}

/** GC 档位。前两个是 ZGC，中间两个是 G1，`none` = 不加任何 GC 参数。 */
export type GcMode = 'zgc' | 'zgc_gen' | 'g1' | 'g1_tuned' | 'none';

/** 版本级 JVM 参数如何与全局参数相处。 */
export type JvmArgsMode = 'append' | 'override';

/** 启动游戏后对启动器自己的动作。 */
export type LaunchAfterAction = 'keep' | 'minimize' | 'hide_to_tray' | 'exit';

export interface VersionSettings {
  memoryMb: number | null;
  javaPath: string | null;
  jvmArgs: string[];
  window: string | null;
  checkBeforeLaunch: boolean | null;
  /** `null` = 继承全局。 */
  launchBrandEnabled: boolean | null;
  /** `null` = 继承全局；空串 = 隐藏。 */
  launchBrand: string | null;
  /** 版本参数怎么和全局参数合：`append` 追加 / `override` 覆盖。 */
  jvmArgsMode: JvmArgsMode;
  /** 这个版本单独指定的 GC；`null` = 跟随全局。 */
  gcMode: GcMode | null;
}

export interface EffectiveConfig {
  memoryMb: number;
  javaPath: string | null;
  jvmArgs: string[];
  window: string;
  checkBeforeLaunch: boolean;
  /** 解析后的标识：`null` = 隐藏。 */
  launchBrand: string | null;
  isolated: boolean;
  isolatedFrom: string;
  /** 合并后的 JVM 参数（已按 `jvmArgsMode` 处理）。 */
  jvmArgsMode: JvmArgsMode;
  gcMode: GcMode;
  highPerformanceGpu: boolean;
  autoChinese: boolean;
  writeLatestLaunchBat: boolean;
}

export interface PluginInfo {
  id: string;
  name: string;
  enabled: boolean;
  state: 'registered' | 'starting' | 'ready' | 'disabled' | 'failed';
  description: string;
  /** 必需插件：后端会拒绝停用，界面据此把开关置灰。 */
  required: boolean;
}

export interface LaunchPlan {
  versionId: string;
  javaPath: string;
  /** `javaPath` 的来源：explicit（设置指定）/ auto（按版本要求挑）/ fallback（PATH 兜底）。 */
  javaSource: 'explicit' | 'auto' | 'fallback' | string;
  gameDir: string;
  assetsDir: string;
  librariesDir: string;
  isolated: boolean;
  mainClass: string;
  classpath: string[];
  jvmArgs: string[];
  gameArgs: string[];
  extraJvmArgs: string[];
  username: string;
  uuid: string;
  assetIndex: string | null;
  /** 生效的游戏内标识；`null` = 这次启动不显示标识。 */
  launcherBrand: string | null;
  launchDir: string;
  nativesDir: string;
  librariesDirs: string[];
  gameVersion: string;
  accessTokenInjected: boolean;
}

/* ---------------------------------------------------------------- *
 * 真实启动 / 崩溃分析
 * ---------------------------------------------------------------- */

export type LaunchPhase =
  | 'idle'
  | 'preparing'
  | 'launching'
  | 'running'
  | 'exited'
  | 'failed'
  | 'cancelled';

/** 启动五步里每一步的状态（和下载阶段共用同一组取值）。 */
export type LaunchStepStatus = 'pending' | 'running' | 'completed' | 'skipped' | 'failed';

/** 一步（PCL2 式进度条的清单项）。顺序固定：check → repair → natives → spawn → window。 */
export interface LaunchStep {
  id: 'check' | 'repair' | 'natives' | 'spawn' | 'window';
  label: string;
  status: LaunchStepStatus;
  /** 这一步正在做什么（文件名 / 阶段文案）。 */
  detail: string;
  doneBytes: number;
  totalBytes: number;
  filesDone: number;
  filesTotal: number;
}

export interface LaunchState {
  /** 本次启动的会话 id；`0` = 还没启动过。多开时靠它区分进程。 */
  sessionId: number;
  phase: LaunchPhase;
  rootId: string;
  versionId: string;
  pid: number | null;
  startedAt: number | null;
  elapsedMs: number;
  exitCode: number | null;
  logPath: string | null;
  consoleTailPath: string | null;
  gameWindowFound: boolean;
  error: string | null;
  crashed: boolean;
  /** 启动说明（GC 降级、自动中文被跳过之类），不是错误。 */
  notes: string[];
  /** 五个阶段的明细。 */
  steps: LaunchStep[];
  /** 加权总进度 0..1（后端算好，界面只负责画）。 */
  percent: number;
  /** 当前阶段的一句话说明。 */
  stageLabel: string;
  cancelled: boolean;
  /** 补全缺失文件用的下载任务 id（下载页能看到同一个任务）。 */
  queueJobId: string | null;
}

/** 启动前检查（`launch_check`）：`ok === false` 时界面必须阻止启动。 */
export interface LaunchCheck {
  ok: boolean;
  blocks: LaunchBlock[];
  notes: string[];
}

export type LaunchBlockKind = 'no-account' | 'auth' | 'java' | 'files' | 'version';

export interface LaunchBlock {
  kind: LaunchBlockKind;
  message: string;
}

export interface CrashKeyword {
  id: string;
  label: string;
  count: number;
}

export type SuspectConfidence = 'low' | 'medium' | 'high';

export interface CrashSuspect {
  fileName: string;
  modId: string | null;
  name: string | null;
  confidence: SuspectConfidence;
  evidence: string;
}

export interface CrashReport {
  detected: boolean;
  exitCode: number | null;
  crashFile: string | null;
  keywords: CrashKeyword[];
  suspects: CrashSuspect[];
  logTail: string;
  capturedAt: number;
}

export interface CrashExport {
  dir: string;
  files: string[];
  manifestPath: string;
}

/** `crash_export_zip`：用户选目录后打成一个 zip。 */
export interface CrashArchive {
  path: string;
  files: number;
  bytes: number;
  fileName: string;
}

/* ---------------------------------------------------------------- *
 * 版本管理 / 存档管理
 * ---------------------------------------------------------------- */

export interface VersionDeleteOutcome {
  versionId: string;
  removedDir: string;
  removedSettings: string | null;
  freedBytes: number;
}

export interface LoaderUpdateStatus {
  loader: LoaderKind;
  current: string | null;
  latest: string | null;
  recommended: string | null;
  newerAvailable: boolean;
  candidates: string[];
}

export interface DataPack {
  fileName: string;
  /** `zip` / `dir` */
  kind: string;
  sizeBytes: number;
  /** `null` = 读不到 `level.dat`（只展示，不写回）。 */
  enabled: boolean | null;
  packFormat: number | null;
  description: string | null;
}

export interface SaveDeleteOutcome {
  folder: string;
  freedBytes: number;
}

/* ---------------------------------------------------------------- *
 * 本地内容（模组 / 资源包 / 存档 / 日志）
 *
 * 位置全部由宿主按「版本隔离」决定，界面不拼路径——`content_dirs` 是
 * 唯一真相来源，其余列表都从它下面读。
 * ---------------------------------------------------------------- */

export type ContentKind = 'mod' | 'resource_pack';

export interface ContentDirs {
  gameDir: string;
  modsDir: string;
  resourcePacksDir: string;
  savesDir: string;
  logsDir: string;
  isolated: boolean;
  /** 隔离设置是从哪一级来的（「文件夹」「全局默认」……），用于解释目录位置。 */
  isolatedFrom: string;
}

export interface LocalMod {
  fileName: string;
  displayName: string;
  version: string | null;
  sizeBytes: number;
  enabled: boolean;
  /** Unix 秒。 */
  modifiedAt: number | null;
  /** jar 内声明的 mod id。 */
  modId: string | null;
  /** jar 内的原始名称（未做中文替换）。 */
  name: string | null;
  authors: string[];
  /** `fabric` / `quilt` / `forge` / `neoforge`。 */
  flavors: string[];
  /** 有图标时用 `content_mod_icon` 按需取图片。 */
  hasIcon: boolean;
  /** 元数据来源：`fabric.mod.json` / `neoforge.mods.toml` / `manifest` / `filename`。 */
  metaSource: string | null;
}

/** 模组图标（base64 data URL）。 */
export interface ModIcon {
  dataUrl: string;
  mime: string;
}

/** 某个已装模组的可用更新。 */
export interface ModUpdate {
  fileName: string;
  modId: string | null;
  /** `modrinth` / `curseforge` */
  source: string;
  projectId: string;
  currentVersion: string | null;
  latestVersion: string;
  latestFileName: string;
  downloadUrl: string;
  size: number;
  sha1: string | null;
  pageUrl: string | null;
}

export interface ModUpdateReport {
  updates: ModUpdate[];
  /** 查不到来源的模组（不算错误）。 */
  unknown: string[];
  warnings: string[];
  checkedAt: number;
}

export interface LocalPack {
  fileName: string;
  displayName: string;
  sizeBytes: number;
  /** 只读：来自 `options.txt` 的 `resourcePacks` 列表。 */
  enabled: boolean;
  /** `options.txt` 里登记了但文件不在了。 */
  missing: boolean;
  isDir: boolean;
  modifiedAt: number | null;
}

export interface LocalSave {
  folder: string;
  name: string;
  /** 中文标签，例如「生存」。 */
  gameMode: string;
  mcVersion: string | null;
  lastPlayed: number | null;
  sizeBytes: number;
  /** 目录太大时提前停止统计，`sizeBytes` 只是下限。 */
  sizeCapped: boolean;
  hasIcon: boolean;
}

export interface LogFile {
  fileName: string;
  sizeBytes: number;
  modifiedAt: number | null;
  isLatest: boolean;
  /** 崩溃日志 / 归档（`.log.gz`）。 */
  compressed: boolean;
}

export interface LogTail {
  fileName: string;
  totalBytes: number;
  shownBytes: number;
  /** 只给了尾部（文件比上限大）。 */
  truncated: boolean;
  text: string;
}

export interface ImportSkipped {
  file: string;
  reason: string;
}

export interface ImportOutcome {
  copied: string[];
  skipped: ImportSkipped[];
}

/* ---------------------------------------------------------------- *
 * Java 运行时（java-runtime 插件）
 * ---------------------------------------------------------------- */

export type JavaSource =
  | 'settings'
  | 'java_home'
  | 'mojang_launcher'
  | 'game_runtime'
  | 'registry'
  | 'common_dir'
  | 'path_var'
  | 'custom';

export interface JavaRuntime {
  path: string;
  home: string;
  name: string;
  version: string;
  major: number;
  vendor: string;
  arch: string;
  source: JavaSource;
  valid: boolean;
}

/** `versions_verify` 的明细。 */
export interface IntegrityReport {
  versionId: string;
  ok: boolean;
  missingJar: boolean;
  missingLibraries: number;
  missingAssetsIndex: boolean;
  missingAssetsObjects: number;
  assetsObjectTotal: number;
  missingTotal: number;
}

/* ---------------------------------------------------------------- *
 * 模组下载（mod-download 插件）
 *
 * 与 `kaleid_api::mods` 一一对应。注意有两条不同的「来源」概念：
 * - `GlobalSettings.modSource` 是**数据源策略**（auto / mcim / official）；
 * - `ModSource` 是**单个条目实际来自哪个站**（modrinth / curseforge）。
 * ---------------------------------------------------------------- */

export type ModSource = 'modrinth' | 'curseforge';
export type ModSourceFilter = 'all' | ModSource;

export interface ModHit {
  source: ModSource;
  /** Modrinth 是 project_id，CurseForge 是数字 id 的字符串。 */
  id: string;
  slug: string;
  name: string;
  /** 本地中文名词表给的译名；没有就是 null。 */
  translatedName: string | null;
  description: string;
  /** MCIM 简介译文；列表里不请求，只有详情页有。 */
  translatedDescription: string | null;
  iconUrl: string | null;
  downloads: number;
  updatedAt: string;
  loaders: string[];
  /** `Fabric / NeoForge` 这类现成标签。 */
  loadersLabel: string;
  /** `1.20+`、`1.16-1.21`。 */
  gameRange: string;
  typeLabel: string;
  tags: string[];
  pageUrl: string;
  wikiUrl: string | null;
}

export interface ModSearchResult {
  hits: ModHit[];
  modrinthTotal: number;
  curseforgeTotal: number;
  warnings: string[];
  fetchedAt: number;
}

export interface ModType {
  id: string;
  label: string;
  /** 大类，界面的分组标题。 */
  group: string;
  cfCategoryId: number | null;
  modrinthSlugs: string[];
}

export interface InstalledMod {
  fileName: string;
  size: number;
  sha1: string | null;
  /** `file_name`（文件名完全相等）或 `slug`（文件名里出现 slug）。 */
  matchedBy: string;
}

export interface ModProject {
  source: ModSource;
  id: string;
  slug: string;
  name: string;
  translatedName: string | null;
  description: string;
  translatedDescription: string | null;
  iconUrl: string | null;
  downloads: number;
  updatedAt: string;
  loadersLabel: string;
  gameRange: string;
  tags: string[];
  pageUrl: string;
  wikiUrl: string | null;
  installed: InstalledMod | null;
}

export interface ModFile {
  source: ModSource;
  projectId: string;
  fileId: string;
  name: string;
  fileName: string;
  versionNumber: string;
  size: number;
  downloads: number;
  publishedAt: string;
  /** release / beta / alpha。 */
  releaseType: string;
  loaders: string[];
  gameVersions: string[];
  officialUrl: string;
  mirrorUrl: string | null;
  sha1: string | null;
  requiredDeps: number;
}

export interface ModVersionGroup {
  loader: string;
  loaderLabel: string;
  /** MC 版本家族（`1.20`）。 */
  gameVersion: string;
  dependencies: ModHit[];
  optionalDependencies: ModHit[];
  files: ModFile[];
}

export interface ModDetail {
  project: ModProject;
  groups: ModVersionGroup[];
  versions: string[];
  warnings: string[];
}

export interface ModTarget {
  rootId: string;
  versionId: string;
  gameVersion: string;
  loader: LoaderKind;
  loaderLabel: string;
  modsDir: string;
  isolated: boolean;
  /** 例如「当前版本是原版，装上的模组不会被加载」。 */
  warning: string | null;
}

export interface ModInstallArgs {
  rootId: string;
  versionId: string;
  source: ModSource;
  projectId: string;
  fileId: string;
  overwrite: boolean;
}

/** Patch payloads for `settings_patch_*` (partial settings objects). */
export type GlobalSettingsPatch = Partial<GlobalSettings>;
export type VersionSettingsPatch = Partial<VersionSettings>;

/* ------------------------------------------------------------------ *
 * 微软登录（设备码流程）
 * ------------------------------------------------------------------ */

/** `msa_config`：client_id 是编译期注入的，可能没配。 */
export interface MsaConfig {
  configured: boolean;
  /** 没配时给用户看的完整句子（说明去 build.env 填哪个键）。 */
  hint: string | null;
}

/** `msa_start`：要展示给用户的设备码。 */
export interface DeviceCodeInfo {
  userCode: string;
  verificationUri: string;
  /** 设备码失效时间（Unix 秒）。 */
  expiresAt: number;
  /** 建议轮询间隔（秒）。 */
  interval: number;
  message: string;
}

/** `msa_poll`：一次轮询的结果。`pending` 是正常状态，不是错误。 */
export type MsaPoll =
  | { state: 'pending'; interval: number; expiresIn: number }
  | { state: 'slowDown'; interval: number }
  | { state: 'done'; account: Account; expiresIn: number };

/** `msa_states`：每个正版账号的令牌状态（只有布尔与时间，没有令牌本身）。 */
export interface MsaAccountState {
  accountId: string;
  hasRefreshToken: boolean;
  expiresAt: number | null;
  /** 这次登录还能用（访问令牌没过期，或者能自动续期）。 */
  usable: boolean;
  /** 微软已经作废了刷新令牌：只能重新登录。 */
  reloginRequired: boolean;
}
