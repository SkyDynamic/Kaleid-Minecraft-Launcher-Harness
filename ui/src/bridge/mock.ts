/**
 * In-memory mock bridge.
 *
 * Used whenever the UI runs outside a Tauri host (plain `pnpm dev` in a
 * browser). It implements the full `KaleidBridge` contract with realistic
 * fixtures and real mutations, so every screen can be exercised without Rust.
 */
import type {
  Account,
  ContentDirs,
  ContentKind,
  EffectiveConfig,
  GlobalSettings,
  GlobalSettingsPatch,
  ImportSkipped,
  IsolationMode,
  JavaRuntime,
  LaunchPlan,
  LoaderKind,
  LoaderVersions,
  LocalMod,
  LocalPack,
  LocalSave,
  LogFile,
  ModDetail,
  ModFile,
  ModHit,
  ModSearchResult,
  ModSource,
  ModType,
  ModVersionGroup,
  PluginInfo,
  RemoteVersion,
  Root,
  RootKind,
  VersionSettings,
  VersionSettingsPatch,
  VersionSummary,
} from './types';

const clone = <T,>(value: T): T => structuredClone(value);
const delay = (ms: number) => new Promise<void>((resolve) => setTimeout(resolve, ms));
const nowIso = () => new Date().toISOString();

const OFFLINE_NAME_RE = /^[A-Za-z0-9_]{1,16}$/;
const OFFLINE_NAME_ERROR = '只能用 1–16 位字母、数字、下划线';

/* ------------------------------------------------------------------ *
 * Fixtures
 * ------------------------------------------------------------------ */

function seedRoots(): Root[] {
  return [
    {
      id: 'roaming',
      path: 'C:\\Users\\SkyDy\\AppData\\Roaming\\.minecraft',
      kind: 'bare_minecraft',
      alias: '默认（Roaming）',
      addedAt: '2026-08-14T10:02:00Z',
      isolation: 'inherit',
      auto: true,
      lastUsed: '2026-10-28T17:40:00Z',
    },
    {
      id: 'exp',
      path: 'D:\\Games\\MC\\Exp\\.minecraft',
      kind: 'parent_with_mc',
      alias: '存档实验场',
      addedAt: '2026-09-02T08:21:00Z',
      isolation: 'on',
      auto: false,
      lastUsed: '2026-10-26T20:12:00Z',
    },
    {
      id: 'fresh',
      path: 'D:\\Temp\\mc-new',
      kind: 'empty_folder',
      alias: '新目录',
      addedAt: '2026-10-20T13:33:00Z',
      isolation: 'inherit',
      auto: false,
      lastUsed: null,
    },
  ];
}

function version(
  id: string,
  loader: LoaderKind,
  loaderVersion: string | null,
  readiness: VersionSummary['readiness'],
  javaMajor: number,
  lastModified: string,
  gameVersion: string | null = null,
): VersionSummary {
  return {
    id,
    loader,
    loaderVersion,
    inheritsFrom: null,
    gameVersion: gameVersion ?? id,
    readiness,
    javaMajor,
    jsonPath: `versions\\${id}\\${id}.json`,
    lastModified,
  };
}

function seedVersions(): Record<string, VersionSummary[]> {
  return {
    roaming: [
      version('1.21.4', 'vanilla', null, 'ready', 21, '2026-10-18T09:12:00Z'),
      version('1.20.1', 'vanilla', null, 'ready', 17, '2026-07-04T11:44:00Z'),
      version('1.19.2', 'vanilla', null, { incomplete: { missing: 3 } }, 17, '2026-05-22T16:08:00Z'),
      version('1.21.4-fabric', 'fabric', '0.16.9', 'ready', 21, '2026-10-27T21:31:00Z', '1.21.4'),
      version('1.21.1-fabric', 'fabric', '0.16.5', 'ready', 21, '2026-09-19T18:26:00Z', '1.21.1'),
      version('1.21.1-neoforge', 'neoforge', '21.1.72', 'ready', 21, '2026-10-02T12:05:00Z', '1.21.1'),
    ],
    exp: [
      version('1.20.4', 'vanilla', null, 'ready', 17, '2026-08-30T10:17:00Z'),
      version('1.20.1-forge-47.2.0', 'forge', '47.2.0', { incomplete: { missing: 1 } }, 17, '2026-08-30T10:19:00Z', '1.20.1'),
      version('1.21.4-fabric', 'fabric', '0.16.9', 'ready', 21, '2026-10-12T23:48:00Z', '1.21.4'),
    ],
    fresh: [],
  };
}

function seedAccounts(): Account[] {
  return [
    {
      id: 'msa-skydy',
      kind: 'msa',
      name: 'SkyDy',
      uuid: '069a79f4-44e9-4726-a5be-fca90e38aaf5',
      skinModel: 'classic',
      skinUrl: null,
      lastUsed: '2026-10-28T17:40:00Z',
    },
    {
      id: 'msa-alex',
      kind: 'msa',
      name: 'Alex_2',
      uuid: '7c9a1b62-9e41-4f8d-b0d2-53f3cb6a1d77',
      skinModel: 'classic',
      skinUrl: null,
      lastUsed: '2026-10-24T11:09:00Z',
    },
    {
      id: 'msa-steve',
      kind: 'msa',
      name: 'Steve_Work',
      uuid: 'b8f6d0c3-1c2a-4b6e-9a55-0e7a2d4c8f13',
      skinModel: 'slim',
      skinUrl: null,
      lastUsed: null,
    },
    {
      id: 'offline-player',
      kind: 'offline',
      name: 'Player_123',
      uuid: '9c1d4f8a-2b67-4a3e-8f10-6d5c7b9e0a24',
      skinModel: 'classic',
      skinUrl: null,
      lastUsed: '2026-10-20T19:55:00Z',
    },
  ];
}

function seedPlugins(): PluginInfo[] {
  const mk = (
    id: string,
    name: string,
    description: string,
    options: { enabled?: boolean; state?: PluginInfo['state']; required?: boolean } = {},
  ): PluginInfo => ({
    id,
    name,
    description,
    enabled: options.enabled ?? true,
    state: options.state ?? 'ready',
    required: options.required ?? false,
  });

  // 与宿主真实装载的插件同 id、同顺序（`standard_preset()`）：浏览器演示里
  // "哪些必需不能关"才会和真机看到的一致，而不是演示一套、真机另一套。
  return [
    mk('core-lists', '文件夹列表', '侧栏文件夹区的兜底实现：只读选择与打开目录', { required: true }),
    mk('version-store', '版本扫描', '扫描 versions/*，解析加载器类型与就绪度', { required: true }),
    mk('settings', '分层设置', '版本 > 文件夹 > 全局 > 内置默认 的配置解析', { required: true }),
    mk('accounts', '账号', '全局账号模型：多个正版账号 + 单个离线档案', { required: true }),
    mk('news', '新闻', '侧栏新闻模块；关闭后不再占位'),
    mk('java-runtime', 'Java 运行时', '发现本机 JDK/JRE 并按版本要求挑选', { required: true }),
    mk('msa-login', '微软登录', 'Microsoft OAuth2 设备码登录，令牌用 DPAPI 加密保存', { required: true }),
    mk('launch', '启动与参数管理', '合并 inheritsFrom、过滤 rules、生成启动命令行', { required: true }),
    mk('game-content', '本地内容', '读取并管理模组 / 资源包 / 存档 / 日志', { required: true }),
    mk('version-manager', '版本管理', '获取新版本：官方源 + BMCLAPI'),
    mk('mod-download', '模组下载', 'Modrinth + CurseForge 搜索、详情、前置与下载'),
  ];
}

/** 与 Rust 侧 `kaleid_api::DEFAULT_JVM_ARGS` 保持一致的默认参数（演示 / 恢复默认用）。 */
const MOCK_DEFAULT_JVM_ARGS = [
  '-XX:-OmitStackTraceInFastThrow',
  '-Djdk.lang.Process.allowAmbiguousCommands=True',
  '-Dfml.ignoreInvalidMinecraftCertificates=True',
  '-Dfml.ignorePatchDiscrepancies=True',
];

function seedGlobal(): GlobalSettings {
  return {
    memoryMb: 4096,
    isolationDefault: true,
    language: 'zh-CN',
    theme: 'dark',
    downloadConcurrency: 8,
    downloadSource: 'official-first',
    modSource: 'official-first',
    modDescription: 'original',
    checkBeforeLaunch: true,
    javaPath: null,
    launchBrandEnabled: true,
    launchBrand: 'Kaleid MC Launcher',
    launchAfterAction: 'keep',
    // 与 Rust 侧 `kaleid_api::DEFAULT_JVM_ARGS` 同内容：设置里这一栏的初始值，
    // 用户可以改、可以删。演示里多给一条好认的，方便看「追加 / 覆盖」的效果。
    jvmArgs: [...MOCK_DEFAULT_JVM_ARGS, '-Dkaleid.demo=1'],
    jvmArgsInitialized: true,
    // 和 Rust 侧的默认值一致：标准 G1GC / 高性能显卡开 / 自动中文开
    gcMode: 'g1',
    highPerformanceGpu: true,
    autoChinese: true,
    writeLatestLaunchBat: true,
  };
}

const EMPTY_VERSION_SETTINGS: VersionSettings = {
  memoryMb: null,
  javaPath: null,
  jvmArgs: [],
  window: null,
  checkBeforeLaunch: null,
  launchBrandEnabled: null,
  launchBrand: null,
  jvmArgsMode: 'append',
  gcMode: null,
};

function remoteVersion(
  id: string,
  category: RemoteVersion['category'],
  releaseTime: string,
  extra: Partial<RemoteVersion> = {},
): RemoteVersion {
  return {
    id,
    type: category === 'release' ? 'release' : 'snapshot',
    releaseTime,
    url: `https://piston-meta.mojang.com/v1/packages/${id}.json`,
    category,
    aprilFoolsYear: null,
    aprilFoolsIntro: null,
    ...extra,
  };
}

const REMOTE_VERSIONS: RemoteVersion[] = [
  remoteVersion('26.1.2', 'release', '2026-04-09T10:00:00+00:00'),
  remoteVersion('26.1.1', 'release', '2026-03-27T12:00:00+00:00'),
  remoteVersion('26.1', 'release', '2026-03-10T09:00:00+00:00'),
  remoteVersion('1.21.4', 'release', '2024-12-03T10:12:00+00:00'),
  remoteVersion('1.20.1', 'release', '2023-06-12T12:00:00+00:00'),
  remoteVersion('26w14a', 'april_fools', '2026-04-01T20:13:30+00:00', {
    aprilFoolsYear: 2026,
    aprilFoolsIntro: '为什么需要物品栏？让方块们跟着你走吧！',
  }),
  remoteVersion('25w14craftmine', 'april_fools', '2025-04-01T18:00:00+00:00', {
    aprilFoolsYear: 2025,
    aprilFoolsIntro: 'Craftmine 更新',
  }),
  remoteVersion('26w13a', 'snapshot', '2026-03-26T10:00:00+00:00'),
  remoteVersion('26w12a', 'snapshot', '2026-03-19T10:00:00+00:00'),
  remoteVersion('b1.7.3', 'ancient', '2011-07-08T12:00:00+00:00'),
  remoteVersion('a1.2.6', 'ancient', '2010-12-02T12:00:00+00:00'),
];

function mockLoaders(
  gameVersion: string,
  loader: 'fabric' | 'forge' | 'neoforge' | 'optifine',
): LoaderVersions {
  const fabric = [
    { version: '0.19.2', stable: true, recommended: true, time: 'Tue, 24 Mar 2026 12:00:00 GMT' },
    { version: '0.19.1', stable: true, recommended: false, time: 'Fri, 06 Mar 2026 12:00:00 GMT' },
    { version: '0.20.0-beta.1', stable: false, recommended: false, time: null },
  ];
  const forge =
    gameVersion === '26.1.2'
      ? [
          { version: '64.1.5', stable: true, recommended: true, time: null },
          { version: '64.1.2', stable: true, recommended: false, time: null },
          { version: '64.0.9', stable: true, recommended: false, time: null },
        ]
      : [
          { version: '54.1.22', stable: true, recommended: true, time: null },
          { version: '54.1.6', stable: true, recommended: false, time: null },
          { version: '54.0.38', stable: true, recommended: false, time: null },
        ];
  const neoforge =
    gameVersion === '26.1'
      ? [
          { version: '26.1.0.19-beta', stable: false, recommended: true, time: null },
          { version: '26.1.0.18-beta', stable: false, recommended: false, time: null },
        ]
      : gameVersion === '26.1.1'
        ? [
            { version: '26.1.1.15-beta', stable: false, recommended: true, time: null },
            { version: '26.1.1.14-beta', stable: false, recommended: false, time: null },
          ]
        : gameVersion === '26.2'
          ? [
              { version: '26.2.0.93', stable: true, recommended: true, time: null },
              { version: '26.2.0.92', stable: true, recommended: false, time: null },
            ]
          : gameVersion === '26.3'
            ? [
                { version: '26.3.0.99', stable: true, recommended: true, time: null },
                { version: '26.3.0.98', stable: true, recommended: false, time: null },
              ]
            : [
                { version: '26.1.2.125', stable: true, recommended: true, time: null },
                { version: '26.1.2.124', stable: true, recommended: false, time: null },
                { version: '26.2.0.8-beta', stable: false, recommended: false, time: null },
              ];
  // OptiFine 的版本号是 `HD_U_J3` 这种形状，BMCLAPI 给的列表由旧到新，
  // 宿主那边会翻成由新到旧，这里直接按新到旧写。
  const optifine =
    gameVersion === '1.12.2'
      ? [
          { version: 'HD_U_G5', stable: true, recommended: true, time: null },
          { version: 'HD_U_F5', stable: true, recommended: false, time: null },
          { version: 'HD_U_E2', stable: true, recommended: false, time: null },
        ]
      : [
          { version: 'HD_U_J3', stable: true, recommended: true, time: null },
          { version: 'HD_U_J2', stable: true, recommended: false, time: null },
          { version: 'HD_U_J4_pre1', stable: false, recommended: false, time: null },
        ];
  const versions =
    loader === 'fabric'
      ? fabric
      : loader === 'forge'
        ? forge
        : loader === 'optifine'
          ? optifine
          : neoforge;
  return {
    gameVersion,
    loader,
    versions,
    recommended: versions.find((v) => v.recommended)?.version ?? null,
    fetchedAt: Math.floor(Date.now() / 1000),
  };
}

/* ------------------------------------------------------------------ *
 * 模组下载（mod-download 插件）演示数据
 *
 * 真机里由宿主 `mods_*` 命令访问 Modrinth / CurseForge；浏览器里没有网络
 * 代理，所以这里放一份形状完全一致的假数据，让搜索、详情、前置、安装确认
 * 四条路径都能点通。
 * ------------------------------------------------------------------ */

/** 演示数据的一页大小，与真机的 `RANKING_PAGE_SIZE`（50）保持一致。 */
const MOCK_PAGE_SIZE = 50;

const MOCK_MOD_TYPES: ModType[] = [
  { id: 'optimization', label: '性能优化', group: '功能性', cfCategoryId: 6814, modrinthSlugs: ['optimization'] },
  { id: 'utility', label: '实用工具', group: '功能性', cfCategoryId: 5191, modrinthSlugs: ['utility'] },
  { id: 'storage', label: '存储', group: '功能性', cfCategoryId: 420, modrinthSlugs: ['storage'] },
  { id: 'library', label: '前置库', group: '依赖', cfCategoryId: 421, modrinthSlugs: ['library'] },
  { id: 'worldgen', label: '世界生成', group: '世界元素', cfCategoryId: 406, modrinthSlugs: ['worldgen'] },
  { id: 'technology', label: '科技', group: '内容扩展', cfCategoryId: 412, modrinthSlugs: ['technology'] },
  { id: 'magic', label: '魔法', group: '内容扩展', cfCategoryId: 419, modrinthSlugs: ['magic'] },
  { id: 'adventure', label: '冒险', group: '内容扩展', cfCategoryId: 422, modrinthSlugs: ['adventure'] },
];

const MOCK_MOD_HITS: ModHit[] = [
  {
    source: 'modrinth',
    id: 'AANobbMI',
    slug: 'sodium',
    name: 'Sodium',
    translatedName: '钠',
    description: '一个现代渲染引擎，显著提升帧数并修复图形问题。',
    translatedDescription: null,
    iconUrl: null,
    downloads: 421_000_000,
    updatedAt: '2026-10-20T08:12:00Z',
    loaders: ['fabric', 'neoforge'],
    loadersLabel: 'Fabric / NeoForge',
    gameRange: '1.16+',
    typeLabel: '性能优化',
    tags: ['性能优化'],
    pageUrl: 'https://modrinth.com/mod/sodium',
    wikiUrl: 'https://search.mcmod.cn/s?key=Sodium',
  },
  {
    source: 'modrinth',
    id: 'LNytGWDc',
    slug: 'create',
    name: 'Create',
    translatedName: '机械动力',
    description: '以旋转动力与机械结构为核心的科技模组，强调可视化与动手搭建。',
    translatedDescription: null,
    iconUrl: null,
    downloads: 312_000_000,
    updatedAt: '2026-10-12T15:40:00Z',
    loaders: ['fabric', 'forge', 'neoforge'],
    loadersLabel: 'Fabric / Forge / NeoForge',
    gameRange: '1.18-1.21',
    typeLabel: '科技',
    tags: ['科技', '实用工具'],
    pageUrl: 'https://modrinth.com/mod/create',
    wikiUrl: 'https://search.mcmod.cn/s?key=Create',
  },
  {
    source: 'curseforge',
    id: '238222',
    slug: 'jei',
    name: 'Just Enough Items (JEI)',
    translatedName: 'JEI 物品管理器',
    description: '查看物品配方与用途，几乎所有整合包都会带一个。',
    translatedDescription: null,
    iconUrl: null,
    downloads: 812_000_000,
    updatedAt: '2026-09-30T06:05:00Z',
    loaders: ['forge', 'fabric', 'neoforge'],
    loadersLabel: 'Forge / Fabric / NeoForge',
    gameRange: '1.12+',
    typeLabel: '实用工具',
    tags: ['实用工具', '前置库'],
    pageUrl: 'https://www.curseforge.com/minecraft/mc-mods/jei',
    wikiUrl: 'https://search.mcmod.cn/s?key=JEI',
  },
  {
    source: 'curseforge',
    id: '223794',
    slug: 'applied-energistics-2',
    name: 'Applied Energistics 2',
    translatedName: '应用能源2',
    description: '以数字化存储与物流为核心的科技模组。',
    translatedDescription: null,
    iconUrl: null,
    downloads: 164_000_000,
    updatedAt: '2026-08-22T11:20:00Z',
    loaders: ['forge', 'fabric'],
    loadersLabel: 'Forge / Fabric',
    gameRange: '1.16-1.21',
    typeLabel: '科技',
    tags: ['科技', '存储'],
    pageUrl: 'https://www.curseforge.com/minecraft/mc-mods/ae2',
    wikiUrl: 'https://search.mcmod.cn/s?key=应用能源2',
  },
  {
    source: 'curseforge',
    id: '227639',
    slug: 'the-twilight-forest',
    name: 'The Twilight Forest',
    translatedName: '暮色森林',
    description: '一个以森林与迷宫探索为主题的大型冒险维度模组。',
    translatedDescription: null,
    iconUrl: null,
    downloads: 121_000_000,
    updatedAt: '2026-07-16T09:00:00Z',
    loaders: ['forge', 'neoforge'],
    loadersLabel: 'Forge / NeoForge',
    gameRange: '1.12-1.21',
    typeLabel: '冒险',
    tags: ['冒险', '世界生成'],
    pageUrl: 'https://www.curseforge.com/minecraft/mc-mods/the-twilight-forest',
    wikiUrl: 'https://search.mcmod.cn/s?key=暮色森林',
  },
  {
    source: 'modrinth',
    id: 'YL57xq9U',
    slug: 'iris',
    name: 'Iris Shaders',
    translatedName: 'Iris 光影',
    description: '为 Fabric 提供光影支持，兼容 OptiFine 光影包。',
    translatedDescription: null,
    iconUrl: null,
    downloads: 238_000_000,
    updatedAt: '2026-10-24T17:31:00Z',
    loaders: ['fabric', 'neoforge'],
    loadersLabel: 'Fabric / NeoForge',
    gameRange: '1.16+',
    typeLabel: '性能优化',
    tags: ['性能优化'],
    pageUrl: 'https://modrinth.com/mod/iris',
    wikiUrl: null,
  },
  {
    source: 'modrinth',
    id: 'P7dR8mSH',
    slug: 'fabric-api',
    name: 'Fabric API',
    translatedName: 'Fabric API',
    description: '绝大多数 Fabric 模组的前置库。',
    translatedDescription: null,
    iconUrl: null,
    downloads: 986_000_000,
    updatedAt: '2026-10-25T12:00:00Z',
    loaders: ['fabric'],
    loadersLabel: 'Fabric',
    gameRange: '1.14+',
    typeLabel: '前置库',
    tags: ['前置库'],
    pageUrl: 'https://modrinth.com/mod/fabric-api',
    wikiUrl: null,
  },
  {
    source: 'curseforge',
    id: '255002',
    slug: 'botania',
    name: 'Botania',
    translatedName: '植物魔法',
    description: '用花与魔力驱动的魔法模组。',
    translatedDescription: null,
    iconUrl: null,
    downloads: 44_000_000,
    updatedAt: '2026-06-02T13:45:00Z',
    loaders: ['forge'],
    loadersLabel: 'Forge',
    gameRange: '1.10-1.20',
    typeLabel: '魔法',
    tags: ['魔法'],
    pageUrl: 'https://www.curseforge.com/minecraft/mc-mods/botania',
    wikiUrl: 'https://search.mcmod.cn/s?key=植物魔法',
  },
];

const MOCK_LOADER_LABELS: Record<string, string> = {
  fabric: 'Fabric',
  forge: 'Forge',
  neoforge: 'NeoForge',
  quilt: 'Quilt',
  universal: '通用',
};

function mockModSearch(args: unknown): ModSearchResult {
  const query = String(arg<string>(args, 'query') ?? '').trim().toLowerCase();
  const source = String(arg<string>(args, 'source') ?? 'all');
  const typeId = arg<string>(args, 'typeId') ?? '';
  const type = MOCK_MOD_TYPES.find((item) => item.id === typeId);

  let hits = MOCK_MOD_HITS.filter((hit) => source === 'all' || hit.source === source);
  if (type) hits = hits.filter((hit) => hit.tags.includes(type.label));
  if (query) {
    hits = hits.filter((hit) =>
      `${hit.name} ${hit.translatedName ?? ''} ${hit.slug} ${hit.description}`
        .toLowerCase()
        .includes(query),
    );
  }
  hits = [...hits].sort((a, b) => b.downloads - a.downloads);

  // 真机上两端都是 50 条一页（排行榜与搜索共用同一页大小），演示数据量很小，
  // 这里只保证"翻页不会把同一批全塞进来"。
  const page = Math.max(0, Number(arg<number>(args, 'page') ?? 0) || 0);
  const pageHits = hits.slice(page * MOCK_PAGE_SIZE, page * MOCK_PAGE_SIZE + MOCK_PAGE_SIZE);

  return {
    hits: mockDescribe(clone(pageHits)),
    modrinthTotal: MOCK_MOD_HITS.filter((hit) => hit.source === 'modrinth').length,
    curseforgeTotal: MOCK_MOD_HITS.filter((hit) => hit.source === 'curseforge').length,
    warnings:
      source === 'modrinth'
        ? []
        : source === 'curseforge'
          ? ['浏览器演示数据；真机上 CurseForge 走 MCIM 镜像']
          : [],
    fetchedAt: Date.now(),
  };
}

/// 演示数据里模拟「Mod 列表注释 = 翻译」：真机上这段话来自 MCIM 翻译接口。
function mockDescribe(hits: ModHit[]): ModHit[] {
  if (state.global.modDescription !== 'translated') return hits;
  return hits.map((hit) => ({
    ...hit,
    translatedDescription: `【MCIM 译文】${hit.description}`,
  }));
}

function mockModDetail(source: ModSource, projectId: string): ModDetail {
  const project = MOCK_MOD_HITS.find((hit) => hit.source === source && hit.id === projectId);
  if (!project) throw new Error('浏览器演示模式里没有这个模组');

  const loaders = project.loaders.length > 0 ? project.loaders : ['fabric'];
  const families = ['1.21', '1.20'];
  const dependencies =
    project.slug === 'iris'
      ? MOCK_MOD_HITS.filter((hit) => hit.slug === 'sodium')
      : project.slug === 'fabric-api'
        ? []
        : project.tags.includes('前置库')
          ? []
          : [];

  const groups: ModVersionGroup[] = [];
  for (const loader of loaders) {
    for (const family of families) {
      const base = `${project.slug}-1.2.0+${family}`;
      const files: ModFile[] = [
        {
          source,
          projectId: project.id,
          fileId: `${base}-${loader}`,
          name: `${project.name} 1.2.0 (${loader} ${family})`,
          fileName: `${base}-${loader}.jar`,
          versionNumber: '1.2.0',
          size: 1_320_000,
          downloads: 1_800_000,
          publishedAt: '2026-10-18T09:00:00Z',
          releaseType: 'release',
          loaders: [loader],
          gameVersions: [family],
          officialUrl: 'https://cdn.modrinth.com/data/example.jar',
          mirrorUrl: null,
          sha1: null,
          requiredDeps: dependencies.length,
        },
        {
          source,
          projectId: project.id,
          fileId: `${base}-${loader}-beta`,
          name: `${project.name} 1.3.0-beta (${loader} ${family})`,
          fileName: `${base}-${loader}-beta.jar`,
          versionNumber: '1.3.0-beta',
          size: 1_410_000,
          downloads: 41_000,
          publishedAt: '2026-10-27T20:10:00Z',
          releaseType: 'beta',
          loaders: [loader],
          gameVersions: [family],
          officialUrl: 'https://cdn.modrinth.com/data/example-beta.jar',
          mirrorUrl: null,
          sha1: null,
          requiredDeps: dependencies.length,
        },
      ];
      groups.push({
        loader,
        loaderLabel: MOCK_LOADER_LABELS[loader] ?? loader,
        gameVersion: family,
        dependencies: clone(dependencies),
        optionalDependencies: [],
        files,
      });
    }
  }

  return {
    project: {
      source: project.source,
      id: project.id,
      slug: project.slug,
      name: project.name,
      translatedName: project.translatedName,
      description: project.description,
      translatedDescription: null,
      iconUrl: project.iconUrl,
      downloads: project.downloads,
      updatedAt: project.updatedAt,
      loadersLabel: project.loadersLabel,
      gameRange: project.gameRange,
      tags: project.tags,
      pageUrl: project.pageUrl,
      wikiUrl: project.wikiUrl,
      installed: null,
    },
    groups,
    versions: families,
    warnings: ['浏览器演示数据，安装不会真的下载文件'],
  };
}

/* ------------------------------------------------------------------ *
 * State
 * ------------------------------------------------------------------ */

interface MockState {
  roots: Root[];
  activeRootId: string;
  defaultRootId: string | null;
  versions: Record<string, VersionSummary[]>;
  global: GlobalSettings;
  versionSettings: Record<string, VersionSettings>;
  accounts: Account[];
  activeAccountId: string;
  plugins: PluginInfo[];
  /** 浏览器里的"运行中"模拟：可以同时有多个会话（多开），外加每个版本被启动过几次。 */
  sessions: MockSession[];
  nextSessionId: number;
  launchCounts: Record<string, number>;
}

const state: MockState = {
  roots: seedRoots(),
  activeRootId: 'roaming',
  defaultRootId: 'roaming',
  versions: seedVersions(),
  global: seedGlobal(),
  versionSettings: {},
  accounts: seedAccounts(),
  activeAccountId: 'msa-skydy',
  plugins: seedPlugins(),
  sessions: [],
  nextSessionId: 1,
  launchCounts: {},
};

const listeners = new Map<string, Set<(payload: unknown) => void>>();

// 微软登录的演示状态：轮询计数 + "哪些账号有令牌"。
// 真机上这些都活在 Rust 侧，这里只是让浏览器 demo 能走完整套界面。
let msaPolls = 0;
let msaPending = false;
const msaTokens = new Set<string>();
// 演示里的预置账号默认"有可用令牌"：这样直接点启动能走通。
// 想复现「登录失效 → 弹原因 → 阻止启动」，在账号卡里点一次退出登录再启动即可。
for (const account of state.accounts) {
  if (account.kind === 'msa') msaTokens.add(account.id);
}

function emit(event: string, payload: unknown) {
  listeners.get(event)?.forEach((cb) => cb(clone(payload)));
}

function versionKey(rootId: string, versionId: string) {
  return `${rootId}::${versionId}`;
}

function requireRoot(id: string): Root {
  const root = state.roots.find((r) => r.id === id);
  if (!root) throw new Error(`未找到文件夹：${id}`);
  return root;
}

/* ------------------------------------------------------------------ *
 * 真实启动 / 崩溃的浏览器模拟
 * ------------------------------------------------------------------ */

/** 浏览器里一个"在跑的游戏进程"。多开时数组里会有多个。 */
interface MockSession {
  sessionId: number;
  rootId: string;
  versionId: string;
  startedAt: number;
  crashed: boolean;
  /** 浏览器演示里"准备阶段"演多久（真机上是补全缺失文件的时间）。 */
  prepMs: number;
  /** 用户点了遮罩上的「取消启动」。 */
  cancelled: boolean;
}

/**
 * 五步的骨架（id/文案和后端 `LAUNCH_STEPS` 一致）。
 *
 * 演示不是随便画个假进度：权重、文案、字节计数都照着后端来，
 * 这样在浏览器里看到的效果和真机是同一套排版。
 */
const MOCK_STEPS: { id: string; label: string }[] = [
  { id: 'check', label: '检查启动条件' },
  { id: 'repair', label: '补全缺失文件' },
  { id: 'natives', label: '解压 natives' },
  { id: 'spawn', label: '启动游戏进程' },
  { id: 'window', label: '等待游戏窗口' },
];

const MOCK_WEIGHTS = [8, 55, 7, 15, 15];

function mockSteps(done: number, runningAt: number | null, repairRatio: number) {
  return MOCK_STEPS.map((step, index) => {
    const status =
      index < done
        ? ('completed' as const)
        : index === runningAt
          ? ('running' as const)
          : ('pending' as const);
    const isRepair = step.id === 'repair';
    return {
      id: step.id as 'check' | 'repair' | 'natives' | 'spawn' | 'window',
      label: step.label,
      status,
      detail:
        status === 'running'
          ? isRepair
            ? `补全缺失文件 · ${Math.round(repairRatio * 57)} / 57`
            : '进行中…'
          : status === 'completed'
            ? '完成'
            : '',
      doneBytes: isRepair ? Math.round(repairRatio * 45 * 1024 * 1024) : 0,
      totalBytes: isRepair ? 45 * 1024 * 1024 : 0,
      filesDone: isRepair ? Math.round(repairRatio * 57) : 0,
      filesTotal: isRepair ? 57 : 0,
    };
  });
}

/** 从启动时刻推演出运行时快照：准备 → 等窗口 → 运行中。 */
function mockLaunchSnapshot(session: MockSession) {
  const elapsed = Date.now() - session.startedAt;
  const base = {
    sessionId: session.sessionId,
    rootId: session.rootId,
    versionId: session.versionId,
    pid: 4200 + session.sessionId,
    startedAt: session.startedAt,
    elapsedMs: elapsed,
    exitCode: null,
    consoleTailPath: null,
    logPath: `C:\\Users\\demo\\AppData\\Roaming\\Kaleid\\logs\\demo-${session.sessionId}\\console.log`,
    error: null,
    crashed: false,
    notes: [] as string[],
    queueJobId: null,
  };

  if (session.cancelled) {
    return {
      ...base,
      phase: 'cancelled' as const,
      pid: null,
      gameWindowFound: false,
      steps: mockSteps(1, null, 0.4),
      percent: 0.08,
      stageLabel: '已取消启动',
      cancelled: true,
    };
  }

  if (elapsed < session.prepMs) {
    const ratio = Math.max(0.02, elapsed / session.prepMs);
    return {
      ...base,
      phase: 'preparing' as const,
      pid: null,
      gameWindowFound: false,
      steps: mockSteps(1, 1, ratio),
      percent:
        (MOCK_WEIGHTS[0] + MOCK_WEIGHTS[1] * ratio) /
        MOCK_WEIGHTS.reduce((a, b) => a + b, 0),
      stageLabel: `补全缺失文件 · ${Math.round(ratio * 57)} / 57`,
      cancelled: false,
    };
  }

  const launching = elapsed < session.prepMs + 1500;
  if (launching) {
    const total = MOCK_WEIGHTS.reduce((a, b) => a + b, 0);
    return {
      ...base,
      phase: 'launching' as const,
      gameWindowFound: false,
      steps: mockSteps(4, 4, 1),
      percent: (total - MOCK_WEIGHTS[4]) / total,
      stageLabel: '等待窗口出现…',
      cancelled: false,
    };
  }

  return {
    ...base,
    phase: 'running' as const,
    gameWindowFound: true,
    steps: mockSteps(5, null, 1),
    percent: 1,
    stageLabel: '游戏已启动',
    cancelled: false,
  };
}

/** 所有会话的快照；正在跑的排前面（和宿主的 `launch_states` 一致）。 */
function mockLaunchStates() {
  return state.sessions.map(mockLaunchSnapshot).sort((a, b) => {
    const busy = (b.phase === 'running' || b.phase === 'launching' ? 1 : 0) -
      (a.phase === 'running' || a.phase === 'launching' ? 1 : 0);
    return busy !== 0 ? busy : b.sessionId - a.sessionId;
  });
}

/** 最近一个会话（`launch_state` 的老语义）。 */
function mockLaunchState() {
  const session = state.sessions[state.sessions.length - 1];
  if (!session) {
    return {
      sessionId: 0,
      phase: 'idle' as const,
      rootId: '',
      versionId: '',
      pid: null,
      startedAt: null,
      elapsedMs: 0,
      exitCode: null,
      logPath: null,
      consoleTailPath: null,
      gameWindowFound: false,
      error: null,
      crashed: false,
      notes: [] as string[],
      steps: mockSteps(0, null, 0),
      percent: 0,
      stageLabel: '还没启动',
      cancelled: false,
      queueJobId: null,
    };
  }
  return mockLaunchSnapshot(session);
}

/** 一份典型的（虚构的）崩溃报告，用来在浏览器里检查弹窗排版与措辞。 */
function mockCrashReport() {
  return {
    detected: true,
    exitCode: 1,
    crashFile: 'crash-reports/crash-2026-10-31_05.20.11-client.txt',
    keywords: [
      { id: 'mixin_apply_failed', label: 'Mixin 注入失败', count: 3 },
      { id: 'no_such_method', label: '找不到方法/字段（模组与游戏版本不匹配）', count: 1 },
    ],
    suspects: [
      {
        fileName: 'sodium-0.5.8.jar',
        modId: 'sodium',
        name: 'sodium',
        confidence: 'high' as const,
        evidence:
          'Mixin apply failed: sodium.mixins.json -> sodium-0.5.8.jar\n\tat net.caffeinemc.mods.sodium.mixin.Foo.apply(Foo.java:41)',
      },
      {
        fileName: 'iris-1.7.0.jar',
        modId: 'iris',
        name: 'iris',
        confidence: 'low' as const,
        evidence: '\tat net.irisshaders.iris.Iris.onInitialize(Iris.java:88)',
      },
    ],
    logTail: [
      '[05:20:09] [Render thread/INFO] (Minecraft) Backend library: LWJGL version 3.3.3+5',
      '[05:20:10] [Render thread/ERROR] (Mixin) Mixin apply failed: sodium.mixins.json -> sodium-0.5.8.jar',
      'java.lang.NoSuchMethodError: net.minecraft.client.renderer.GameRenderer.getFov()D',
      '[05:20:11] [Render thread/FATAL] (Minecraft) Crash report saved to crash-reports/crash-2026-10-31_05.20.11-client.txt',
    ].join('\n'),
    capturedAt: Date.now(),
  };
}

/** 存档里的数据包演示数据。 */
function mockDatapacks() {
  return [
    {
      fileName: 'MyPack.zip',
      kind: 'zip',
      sizeBytes: 42_311,
      enabled: true,
      packFormat: 61,
      description: '自定义合成配方',
    },
    {
      fileName: 'BetterStructures',
      kind: 'dir',
      sizeBytes: 921_600,
      enabled: false,
      packFormat: 48,
      description: '更多结构',
    },
    {
      fileName: 'vanilla',
      kind: 'dir',
      sizeBytes: 0,
      enabled: null,
      packFormat: null,
      description: null,
    },
  ];
}
function requireVersion(rootId: string, versionId: string): VersionSummary {
  const found = (state.versions[rootId] ?? []).find((v) => v.id === versionId);
  if (!found) throw new Error(`未找到版本：${versionId}`);
  return found;
}

function activeAccount(): Account | null {
  return state.accounts.find((a) => a.id === state.activeAccountId) ?? null;
}

function resolveIsolation(root: Root): boolean {
  if (root.isolation === 'on') return true;
  if (root.isolation === 'off') return false;
  return state.global.isolationDefault;
}

function effectiveFor(root: Root, versionId: string): EffectiveConfig {
  const stored = state.versionSettings[versionKey(root.id, versionId)] ?? EMPTY_VERSION_SETTINGS;
  const brandEnabled = stored.launchBrandEnabled ?? state.global.launchBrandEnabled;
  const brandText = (stored.launchBrand ?? state.global.launchBrand).trim();
  const jvmArgsMode = stored.jvmArgsMode ?? 'append';
  const jvmArgs =
    jvmArgsMode === 'override'
      ? [...stored.jvmArgs]
      : [...state.global.jvmArgs, ...stored.jvmArgs];
  return {
    memoryMb: stored.memoryMb ?? state.global.memoryMb,
    javaPath: stored.javaPath ?? state.global.javaPath,
    jvmArgs,
    window: stored.window ?? '1280x720',
    checkBeforeLaunch: stored.checkBeforeLaunch ?? state.global.checkBeforeLaunch,
    launchBrand: brandEnabled && brandText ? brandText : null,
    isolated: resolveIsolation(root),
    isolatedFrom: root.isolation === 'inherit' ? '全局默认' : '本文件夹',
    jvmArgsMode,
    gcMode: stored.gcMode ?? state.global.gcMode,
    highPerformanceGpu: state.global.highPerformanceGpu,
    autoChinese: state.global.autoChinese,
    writeLatestLaunchBat: state.global.writeLatestLaunchBat,
  };
}

function inferRootKind(path: string): RootKind {
  const normalized = path.replace(/[\\/]+$/, '').toLowerCase();
  if (normalized.endsWith('\\.minecraft') || normalized.endsWith('/.minecraft')) return 'bare_minecraft';
  if (/[\\/]mc$/.test(normalized) || /[\\/]minecraft$/.test(normalized)) return 'parent_with_mc';
  return 'empty_folder';
}

function aliasFromPath(path: string): string {
  const parts = path.replace(/[\\/]+$/, '').split(/[\\/]/);
  return parts[parts.length - 1] || path;
}

function offlineUuid(name: string): string {
  let h1 = 0x811c9dc5;
  let h2 = 0x1000193;
  for (let i = 0; i < name.length; i++) {
    const c = name.charCodeAt(i);
    h1 = Math.imul(h1 ^ c, 0x01000193) >>> 0;
    h2 = Math.imul(h2 + c, 0x85ebca6b) >>> 0;
  }
  const hex = (h1.toString(16) + h2.toString(16)).padEnd(32, '0').slice(0, 32);
  return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-4${hex.slice(13, 16)}-8${hex.slice(17, 20)}-${hex.slice(20, 32)}`;
}

function launchMainClass(loader: LoaderKind): string {
  switch (loader) {
    case 'fabric':
      return 'net.fabricmc.loader.impl.launch.knot.KnotClient';
    case 'forge':
      return 'net.minecraftforge.bootstrap.ForgeBootstrap';
    case 'neoforge':
      return 'cpw.mods.bootstraplauncher.BootstrapLauncher';
    // 低版本单装 OptiFine 走 launchwrapper + OptiFineTweaker。
    case 'optifine':
      return 'net.minecraft.launchwrapper.Launch';
    default:
      return 'net.minecraft.client.main.Main';
  }
}

/* ------------------------------------------------------------------ *
 * 本地内容（模组 / 资源包 / 存档 / 日志）的演示数据
 *
 * 浏览器里没有磁盘，但**行为**要跟宿主一致：启停改名、导入不覆盖同名、
 * 删除真的从数组里去掉。这样界面逻辑（尤其是错误分支）能在浏览器里验。
 * ------------------------------------------------------------------ */

interface MockContent {
  mods: LocalMod[];
  packs: LocalPack[];
  saves: LocalSave[];
  logs: LogFile[];
  /** 文件名 → 内容（预览用）。 */
  logText: Record<string, string>;
}

const DEMO_LOG_TEXT = [
  '[01:12:04.318] [INFO]  kaleid::launch  preparing launch plan for 1.21.4-fabric',
  '[01:12:04.319] [INFO]  kaleid::config  memory=4096MB java=Temurin 21.0.5 (auto)',
  '[01:12:04.321] [INFO]  kaleid::plugin::version-manager  scanned 6 versions under Roaming',
  '[01:12:04.402] [DEBUG] kaleid::auth  active account SkyDy (msa)',
  '[01:12:04.405] [INFO]  kaleid::plugin::launch  classpath=47 entries',
  '[01:12:04.418] [DEBUG] kaleid::fs  isolated dir = versions\\1.21.4-fabric',
  '[01:12:04.431] [INFO]  kaleid::plugin::launch  spawning java -Xmx4096M -XX:+UseG1GC',
  '[01:12:05.006] [INFO]  kaleid::plugin::launch  game process started (pid 24188)',
  '[01:12:05.007] [INFO]  kaleid::kernel  launch completed in 689ms',
].join('\n');

const contentStores = new Map<string, MockContent>();

function contentKey(rootId: string, versionId: string) {
  return `${rootId}::${versionId}`;
}

function seedContent(): MockContent {
  return {
    mods: [
      ['sodium-fabric-0.6.9.jar', 'Sodium', '0.6.9', 1_947_238, true, 'sodium'],
      ['lithium-fabric-0.15.3.jar', 'Lithium', '0.15.3', 626_688, true, 'lithium'],
      ['iris-fabric-1.8.8.jar', 'Iris Shaders', '1.8.8', 9_856_614, true, 'iris'],
      ['fabric-api-0.115.6+1.21.4.jar', 'Fabric API', '0.115.6+1.21.4', 1_782_579, true, 'fabric-api'],
      ['modmenu-13.0.3.jar', 'Mod Menu', '13.0.3', 831_488, true, 'modmenu'],
      ['indium-1.0.36.jar', 'Indium', '1.0.36', 151_552, false, 'indium'],
      ['xaeros-minimap-25.1.0.jar', "Xaero's Minimap", '25.1.0', 2_726_297, true, 'xaeros-minimap'],
      ['jade-15.10.2.jar', 'Jade', '15.10.2', 1_153_433, false, 'jade'],
    ].map(([fileName, displayName, version, sizeBytes, enabled, modId]) => ({
      fileName: enabled ? (fileName as string) : `${fileName}.disabled`,
      displayName: displayName as string,
      version: version as string,
      sizeBytes: sizeBytes as number,
      enabled: enabled as boolean,
      modifiedAt: 1_761_800_000,
      modId: modId as string,
      name: displayName as string,
      authors: ['Mock Author'],
      flavors: ['fabric'],
      hasIcon: true,
      metaSource: 'fabric.mod.json',
    })),
    packs: [
      { fileName: 'Stay_True_1.21.4.zip', displayName: 'Stay True', sizeBytes: 6_081_331, enabled: true, missing: false, isDir: false, modifiedAt: 1_761_700_000 },
      { fileName: 'FreshAnimations_v1.9.zip', displayName: 'Fresh Animations', sizeBytes: 1_258_291, enabled: true, missing: false, isDir: false, modifiedAt: 1_761_600_000 },
      { fileName: '纯白界面优化.zip', displayName: '纯白界面优化', sizeBytes: 348_160, enabled: false, missing: false, isDir: false, modifiedAt: 1_761_500_000 },
      { fileName: '已删掉的包.zip', displayName: '已删掉的包', sizeBytes: 0, enabled: true, missing: true, isDir: false, modifiedAt: null },
    ],
    saves: [
      { folder: '新世界', name: '新世界', gameMode: '生存', mcVersion: '1.21.4', lastPlayed: 1_761_509_640, sizeBytes: 50_540_544, sizeCapped: false, hasIcon: true },
      { folder: '红石实验场', name: '红石实验场', gameMode: '创造', mcVersion: '1.21.4', lastPlayed: 1_761_172_920, sizeBytes: 117_440_512, sizeCapped: false, hasIcon: true },
      { folder: 'survival-121', name: '生存-1.21', gameMode: '生存', mcVersion: '1.21.1', lastPlayed: 1_760_338_020, sizeBytes: 67_108_864, sizeCapped: true, hasIcon: false },
    ],
    logs: [
      { fileName: 'latest.log', sizeBytes: DEMO_LOG_TEXT.length, modifiedAt: 1_761_845_524, isLatest: true, compressed: false },
      { fileName: 'debug.log', sizeBytes: 41_288_601, modifiedAt: 1_761_845_100, isLatest: false, compressed: false },
      { fileName: '2026-10-27-1.log.gz', sizeBytes: 189_336, modifiedAt: 1_761_590_400, isLatest: false, compressed: true },
    ],
    logText: {
      'latest.log': DEMO_LOG_TEXT,
      'debug.log': `${DEMO_LOG_TEXT}\n[01:12:05.101] [DEBUG] kaleid::fs  scanning saves directory\n[01:12:05.140] [DEBUG] kaleid::fs  found 3 worlds`,
      '2026-10-27-1.log.gz': '（这是压缩归档日志的演示内容）\n上次启动崩溃：java.lang.OutOfMemoryError: Java heap space',
    },
  };
}

function contentFor(rootId: string, versionId: string): MockContent {
  const key = contentKey(rootId, versionId);
  let store = contentStores.get(key);
  if (!store) {
    store = seedContent();
    contentStores.set(key, store);
  }
  return store;
}

function mockDirs(root: Root, versionId: string): ContentDirs {
  const isolated = resolveIsolation(root);
  const gameDir = isolated ? `${root.path}\\versions\\${versionId}` : root.path;
  return {
    gameDir,
    modsDir: `${gameDir}\\mods`,
    resourcePacksDir: `${gameDir}\\resourcepacks`,
    savesDir: `${gameDir}\\saves`,
    logsDir: `${gameDir}\\logs`,
    isolated,
    isolatedFrom: root.isolation === 'inherit' ? '全局默认' : '本文件夹',
  };
}

function mockJavaList(): JavaRuntime[] {
  return [
    { path: 'C:\\Program Files\\Eclipse Adoptium\\jdk-21.0.5+11-hotspot\\bin\\javaw.exe', home: 'C:\\Program Files\\Eclipse Adoptium\\jdk-21.0.5+11-hotspot', name: 'Java 21', version: '21.0.5', major: 21, vendor: 'Eclipse Adoptium', arch: 'x86_64', source: 'registry', valid: true },
    { path: 'C:\\Program Files\\Microsoft\\jdk-17.0.13.11-hotspot\\bin\\javaw.exe', home: 'C:\\Program Files\\Microsoft\\jdk-17.0.13.11-hotspot', name: 'Java 17', version: '17.0.13.11', major: 17, vendor: 'Microsoft', arch: 'x86_64', source: 'common_dir', valid: true },
    { path: 'C:\\Users\\SkyDy\\AppData\\Local\\Packages\\Microsoft.4297127D64EC6_8wekyb3d8bbwe\\LocalCache\\Local\\runtime\\java-runtime-delta\\windows-x64\\java-runtime-delta\\bin\\javaw.exe', home: 'C:\\Users\\SkyDy\\AppData\\Local\\Packages\\Microsoft.4297127D64EC6_8wekyb3d8bbwe\\LocalCache\\Local\\runtime\\java-runtime-delta\\windows-x64\\java-runtime-delta', name: 'Java 21', version: '21.0.3', major: 21, vendor: 'Microsoft', arch: 'x86_64', source: 'mojang_launcher', valid: true },
    { path: 'C:\\Program Files\\Java\\jre1.8.0_401\\bin\\javaw.exe', home: 'C:\\Program Files\\Java\\jre1.8.0_401', name: 'Java 8', version: '1.8.0_401', major: 8, vendor: 'Oracle', arch: 'x86_64', source: 'common_dir', valid: true },
  ];
}

/** 自动匹配：精确 → 最近的更高 → 最高的可用版本（跟宿主同一套规则）。 */
function mockPickJava(major: number | null): JavaRuntime | null {
  const usable = mockJavaList();
  if (major === null) return usable[0] ?? null;
  const exact = usable.find((r) => r.major === major);
  if (exact) return exact;
  const higher = usable.filter((r) => r.major > major).sort((a, b) => a.major - b.major);
  if (higher.length > 0) return higher[0];
  return usable.sort((a, b) => b.major - a.major)[0] ?? null;
}

/* ------------------------------------------------------------------ *
 * Command dispatch
 * ------------------------------------------------------------------ */

type Args = Record<string, unknown>;

function arg<T>(args: unknown, key: string): T | undefined {
  return (args as Args | undefined)?.[key] as T | undefined;
}

async function dispatch(cmd: string, args?: unknown): Promise<unknown> {
  switch (cmd) {
    case 'roots_list': {
      // 默认目录置顶，与宿主 `RootRegistry::list()` 的顺序保持一致。
      const list = clone(state.roots);
      if (state.defaultRootId !== null) {
        const idx = list.findIndex((r) => r.id === state.defaultRootId);
        if (idx > 0) list.unshift(list.splice(idx, 1)[0]);
      }
      return list;
    }

    case 'roots_default':
      return clone(state.roots.find((r) => r.id === state.defaultRootId) ?? null);

    case 'roots_set_default': {
      const root = requireRoot(String(arg<string>(args, 'id')));
      state.defaultRootId = root.id;
      return clone(root);
    }

    case 'roots_clear_default':
      state.defaultRootId = null;
      return null;

    case 'roots_add': {
      const path = String(arg<string>(args, 'path') ?? '').trim();
      if (!path) throw new Error('路径不能为空');
      if (state.roots.some((r) => r.path.toLowerCase() === path.toLowerCase()) && !arg<boolean>(args, 'force')) {
        throw new Error('该目录已在列表中');
      }
      const root: Root = {
        id: `root-${Date.now().toString(36)}`,
        path,
        kind: inferRootKind(path),
        alias: (arg<string>(args, 'alias') ?? '').trim() || aliasFromPath(path),
        addedAt: nowIso(),
        isolation: 'inherit',
        auto: false,
        lastUsed: null,
      };
      state.roots.push(root);
      state.versions[root.id] = [];
      state.activeRootId = root.id;
      return clone(root);
    }

    case 'roots_remove': {
      const id = String(arg<string>(args, 'id'));
      state.roots = state.roots.filter((r) => r.id !== id);
      delete state.versions[id];
      if (state.activeRootId === id) state.activeRootId = state.roots[0]?.id ?? '';
      return null;
    }

    case 'roots_rename': {
      const id = String(arg<string>(args, 'id'));
      const alias = String(arg<string>(args, 'alias') ?? '').trim();
      if (!alias) throw new Error('别名不能为空');
      const root = requireRoot(id);
      root.alias = alias;
      return clone(root);
    }

    case 'roots_select': {
      const root = requireRoot(String(arg<string>(args, 'id')));
      root.lastUsed = nowIso();
      state.activeRootId = root.id;
      return clone(root);
    }

    case 'roots_set_isolation': {
      const root = requireRoot(String(arg<string>(args, 'id')));
      root.isolation = arg<IsolationMode>(args, 'mode') ?? 'inherit';
      return clone(root);
    }

    case 'versions_scan':
      return clone(state.versions[String(arg<string>(args, 'rootId'))] ?? []);

    // ---- 真实启动（浏览器里模拟进程的生命周期，可以多开） ----
    case 'launch_check': {
      const root = requireRoot(String(arg<string>(args, 'rootId')));
      const versionId = String(arg<string>(args, 'versionId'));
      requireVersion(root.id, versionId);
      // 演示里"登录失效"用一个开关演：把 msaTokens 清空后就复现弹窗。
      const account = state.accounts.find((item) => item.id === state.activeAccountId);
      if (!account) {
        return {
          ok: false,
          blocks: [{ kind: 'no-account', message: '还没有登录账号，先在概览的登录卡里选一个' }],
          notes: [],
        };
      }
      if (account.kind === 'msa' && !msaTokens.has(account.id)) {
        return {
          ok: false,
          blocks: [
            {
              kind: 'auth',
              message: '这个微软账号没有可用的登录令牌，请重新登录',
            },
          ],
          notes: [],
        };
      }
      return { ok: true, blocks: [], notes: [] };
    }

    case 'launch_start': {
      const root = requireRoot(String(arg<string>(args, 'rootId')));
      const versionId = String(arg<string>(args, 'versionId'));
      requireVersion(root.id, versionId);
      // 多开：不再拦"已经在运行"
      state.launchCounts[versionId] = (state.launchCounts[versionId] ?? 0) + 1;
      const session: MockSession = {
        sessionId: state.nextSessionId++,
        rootId: root.id,
        versionId,
        startedAt: Date.now(),
        crashed: false,
        // 演示里准备 3.2 秒：够看清进度条在动，又不至于让人干等。
        prepMs: 3200,
        cancelled: false,
      };
      state.sessions.push(session);
      return mockLaunchSnapshot(session);
    }

    case 'launch_cancel': {
      const sessionId = Number(arg<number>(args, 'sessionId'));
      const session = state.sessions.find((s) => s.sessionId === sessionId);
      if (!session) throw new Error('这个启动会话已经不存在了');
      const snapshot = mockLaunchSnapshot(session);
      if (snapshot.phase !== 'preparing') {
        throw new Error('游戏进程已经起来了，要停下请点「结束进程」');
      }
      session.cancelled = true;
      return mockLaunchSnapshot(session);
    }

    case 'launch_console_tail': {
      const want = arg<number>(args, 'sessionId');
      const session = state.sessions.find((s) => s.sessionId === want) ?? state.sessions.at(-1);
      if (!session) throw new Error('还没有启动过游戏，没有日志');
      const text = [
        '[20:45:01] [main/INFO]: Kaleid 启动器：正在准备启动',
        `[20:45:01] [main/INFO]: 版本 ${session.versionId}`,
        '[20:45:02] [main/INFO]: 补全缺失文件 57 个',
        '[20:45:06] [main/INFO]: Launching game',
        '[20:45:09] [Render thread/INFO]: Setting user: Steve',
        '[20:45:14] [Render thread/INFO]: Backend library: LWJGL version 3.3.3',
        '[20:45:18] [Render thread/INFO]: Narrator library for x64 successfully loaded',
        '[20:45:22] [Render thread/INFO]: 游戏窗口已出现',
      ].join('\n');
      return {
        fileName: `${session.versionId}-console.log`,
        totalBytes: text.length,
        shownBytes: text.length,
        truncated: false,
        text,
      };
    }

    case 'crash_export_zip': {
      const targetDir = String(arg<string>(args, 'targetDir') ?? 'C:\\Users\\demo\\Desktop');
      const now = new Date();
      const pad = (n: number) => String(n).padStart(2, '0');
      const fileName = `崩溃日志-${now.getFullYear()}_${pad(now.getMonth() + 1)}_${pad(now.getDate())} ${pad(now.getHours())}-${pad(now.getMinutes())}-${pad(now.getSeconds())}.zip`;
      return {
        path: `${targetDir}\\${fileName}`,
        files: 6,
        bytes: 1_486_752,
        fileName,
      };
    }

    case 'launch_state':
      return mockLaunchState();

    case 'launch_states':
      return mockLaunchStates();

    case 'launch_stop': {
      const want = arg<number>(args, 'sessionId');
      const targets = want ? state.sessions.filter((s) => s.sessionId === want) : state.sessions;
      const stopped = targets.map((session) => {
        // 浏览器 demo：同一个版本第三次启动时演一次崩溃，方便看崩溃弹窗。
        const crashed = (state.launchCounts[session.versionId] ?? 0) >= 3;
        return {
          ...mockLaunchSnapshot(session),
          phase: crashed ? ('failed' as const) : ('exited' as const),
          exitCode: crashed ? 1 : 0,
          crashed,
          elapsedMs: 1000,
        };
      });
      state.sessions = state.sessions.filter((s) => !targets.includes(s));
      return stopped;
    }

    case 'crash_report':
      return mockCrashReport();

    case 'crash_export':
      // 导出时会一并带上 LatestLaunch.bat（真机上和排障包放在同一个目录）
      return {
        dir: 'C:\\Users\\demo\\AppData\\Roaming\\Kaleid\\crash-exports\\demo',
        files: [
          'latest.log',
          'last-log-before-crash.txt',
          'crash.txt',
          'launch-script.txt',
          'LatestLaunch.bat',
          'manifest.json',
        ],
        manifestPath: 'C:\\Users\\demo\\AppData\\Roaming\\Kaleid\\crash-exports\\demo\\manifest.json',
      };

    // ---- 存档管理 ----
    case 'saves_list': {
      const root = requireRoot(String(arg<string>(args, 'rootId')));
      const versionId = String(arg<string>(args, 'versionId'));
      return contentFor(root.id, versionId).saves.map((save) => ({
        folder: save.folder,
        displayName: save.name,
        lastPlayed: save.lastPlayed === null ? null : save.lastPlayed * 1000,
        sizeBytes: save.sizeBytes,
        gameMode: null,
        versionName: save.mcVersion,
        iconDataUrl: null,
        datapackCount: 1,
      }));
    }

    case 'saves_delete': {
      const root = requireRoot(String(arg<string>(args, 'rootId')));
      const folder = String(arg<string>(args, 'folder'));
      const versionId = String(arg<string>(args, 'versionId'));
      const saves = contentFor(root.id, versionId).saves;
      const index = saves.findIndex((s) => s.folder === folder);
      if (index < 0) throw new Error(`存档不存在：${folder}`);
      const [removed] = saves.splice(index, 1);
      return { folder, freedBytes: removed.sizeBytes };
    }

    case 'saves_datapacks_list':
      return mockDatapacks();

    case 'saves_datapack_import': {
      const source = String(arg<string>(args, 'source'));
      return source.split(/[\\/]/).pop() ?? 'pack.zip';
    }

    case 'saves_datapack_delete':
      return null;

    case 'versions_delete': {
      const rootId = String(arg<string>(args, 'rootId'));
      const versionId = String(arg<string>(args, 'versionId'));
      const list = state.versions[rootId] ?? [];
      const index = list.findIndex((v) => v.id === versionId);
      if (index < 0) throw new Error(`版本 ${versionId} 不存在`);
      list.splice(index, 1);
      return {
        versionId,
        removedDir: `versions/${versionId}`,
        removedSettings: null,
        freedBytes: 26 * 1024 * 1024,
      };
    }

    case 'versions_loader_status': {
      const rootId = String(arg<string>(args, 'rootId'));
      const versionId = String(arg<string>(args, 'versionId'));
      const version = (state.versions[rootId] ?? []).find((v) => v.id === versionId);
      if (!version) throw new Error(`版本 ${versionId} 不存在`);
      const vanilla = version.loader === 'vanilla';
      const current = version.loaderVersion ?? null;
      const candidates = vanilla
        ? []
        : [current ?? '0.17.2', '0.17.0', '0.16.10', '0.16.9', '0.16.5'].filter(
            (v, i, all) => all.indexOf(v) === i,
          );
      return {
        loader: version.loader,
        current,
        latest: vanilla ? null : (candidates[0] ?? null),
        recommended: vanilla ? null : (candidates[1] ?? candidates[0] ?? null),
        newerAvailable: false,
        candidates,
      };
    }

    case 'versions_update_loader': {
      const rootId = String(arg<string>(args, 'rootId'));
      const versionId = String(arg<string>(args, 'versionId'));
      const target = arg<string>(args, 'version');
      const version = (state.versions[rootId] ?? []).find((v) => v.id === versionId);
      if (!version) throw new Error(`版本 ${versionId} 不存在`);
      if (!target) throw new Error('没有选择目标加载器版本');
      if (version.loader === 'vanilla') throw new Error('这个版本没有安装模组加载器');
      version.loaderVersion = target;
      return { jobId: `mock-loader-${Date.now().toString(36)}` };
    }

    case 'versions_remote':
      return {
        latestRelease: '26.1.2',
        latestSnapshot: '26w14a',
        versions: clone(REMOTE_VERSIONS),
        fetchedAt: Math.floor(Date.now() / 1000),
      };

    case 'versions_check_name': {
      const rootId = String(arg<string>(args, 'rootId'));
      const name = String(arg<string>(args, 'name') ?? '');
      if (!name) return { ok: false, error: '版本名称不能为空' };
      if (name.endsWith('.')) return { ok: false, error: '文件名不能以小数点结尾' };
      if (/[<>:"/\\|?*]/.test(name)) return { ok: false, error: '名称不能包含 < > : " / \\ | ? *' };
      if ((state.versions[rootId] ?? []).some((v) => v.id.toLowerCase() === name.toLowerCase())) {
        return { ok: false, error: '该版本名称与现有文件夹重名' };
      }
      return { ok: true, error: null };
    }

    case 'versions_install':
      throw new Error('浏览器演示模式没有真实的下载/安装后端，请在 Tauri 桌面外壳中安装。');

    case 'loaders_versions': {
      const gameVersion = String(arg<string>(args, 'gameVersion'));
      const loader = arg<'fabric' | 'forge' | 'neoforge' | 'optifine'>(args, 'loader') ?? 'fabric';
      return mockLoaders(gameVersion, loader);
    }

    case 'loader_version_times': {
      const loader = String(arg<string>(args, 'loader'));
      const versions = arg<string[]>(args, 'versions') ?? [];
      const times: Record<string, string | null> = {};
      for (const [index, version] of versions.entries()) {
        times[version] =
          loader === 'fabric'
            ? 'Tue, 24 Mar 2026 12:00:00 GMT'
            : new Date(Date.UTC(2026, 3, 9, 10, 0, 0) - index * 86_400_000).toUTCString();
      }
      return { times };
    }

    case 'fabric_api_latest': {
      const game = String(arg<string>(args, 'gameVersion') ?? '');
      // 演示数据只对 26.1.x 有 Fabric API；其它版本诚实返回 null。
      if (!game.startsWith('26.1')) return null;
      return {
        version: '0.155.3+26.1.2',
        fileName: 'fabric-api-0.155.3+26.1.2.jar',
        downloadUrl: 'https://cdn.modrinth.com/data/P7dR8mSH/versions/example/fabric-api.jar',
        size: 2521340,
        sha1: '16c30dfcceaed1ac3b6ac84cbd348a906dc5093e',
      };
    }

    case 'skin_resolve':
      // 浏览器演示没有真实账号资料；返回 null，让界面使用内置 Steve。
      return null;

    case 'downloads_snapshot':
      return {
        jobs: [],
        doneBytes: 0,
        totalBytes: 0,
        percent: 0,
        speedBps: 0,
        concurrency: state.global.downloadConcurrency,
        activeWorkers: 0,
        activeJobs: 0,
        queuedJobs: 0,
      };

    case 'downloads_clear_finished':
      return null;

    case 'downloads_cancel':
      throw new Error('浏览器演示模式没有可取消的任务');

    case 'settings_global':
      return clone(state.global);

    case 'jvm_default_args':
      return [...MOCK_DEFAULT_JVM_ARGS];

    // 演示环境一律按 Windows 报：这里跑的是桌面浏览器，没有别的系统要模拟
    case 'app_platform':
      return { os: 'windows', highPerformanceGpuSupported: true };

    case 'settings_patch_global': {
      const patch = (arg<GlobalSettingsPatch>(args, 'patch') ?? {}) as GlobalSettingsPatch;
      state.global = { ...state.global, ...patch };
      return clone(state.global);
    }

    case 'settings_version': {
      const rootId = String(arg<string>(args, 'rootId'));
      const versionId = String(arg<string>(args, 'versionId'));
      const stored = state.versionSettings[versionKey(rootId, versionId)];
      return clone(stored ?? EMPTY_VERSION_SETTINGS);
    }

    case 'settings_patch_version': {
      const rootId = String(arg<string>(args, 'rootId'));
      const versionId = String(arg<string>(args, 'versionId'));
      requireVersion(rootId, versionId);
      const key = versionKey(rootId, versionId);
      const patch = (arg<VersionSettingsPatch>(args, 'patch') ?? {}) as VersionSettingsPatch;
      const merged: VersionSettings = { ...(state.versionSettings[key] ?? EMPTY_VERSION_SETTINGS), ...patch };
      state.versionSettings[key] = merged;
      return clone(merged);
    }

    case 'settings_effective': {
      const root = requireRoot(String(arg<string>(args, 'rootId')));
      return effectiveFor(root, String(arg<string>(args, 'versionId')));
    }

    case 'accounts_list':
      return clone(state.accounts);

    case 'accounts_active':
      return clone(activeAccount());

    case 'accounts_set_active': {
      const id = String(arg<string>(args, 'id'));
      const account = state.accounts.find((a) => a.id === id);
      if (!account) throw new Error(`未找到账号：${id}`);
      account.lastUsed = nowIso();
      state.activeAccountId = account.id;
      return clone(account);
    }

    case 'accounts_apply_offline': {
      const name = String(arg<string>(args, 'name') ?? '');
      if (!OFFLINE_NAME_RE.test(name)) throw new Error(OFFLINE_NAME_ERROR);
      const existing = state.accounts.find((a) => a.kind === 'offline');
      if (existing) {
        existing.name = name;
        existing.uuid = offlineUuid(name);
        existing.lastUsed = nowIso();
        state.activeAccountId = existing.id;
        return clone(existing);
      }
      const created: Account = {
        id: 'offline-player',
        kind: 'offline',
        name,
        uuid: offlineUuid(name),
        skinModel: 'classic',
        skinUrl: null,
        lastUsed: nowIso(),
      };
      state.accounts.push(created);
      state.activeAccountId = created.id;
      return clone(created);
    }

    case 'accounts_remove': {
      const id = String(arg<string>(args, 'id'));
      state.accounts = state.accounts.filter((a) => a.id !== id);
      // 与宿主一致：删账号时顺手丢掉它的令牌
      msaTokens.delete(id);
      if (state.activeAccountId === id) {
        state.activeAccountId = state.accounts.find((a) => a.kind === 'msa')?.id ?? state.accounts[0]?.id ?? '';
      }
      return null;
    }

    /* ---- 微软登录（浏览器里演一遍设备码流程）---- */

    case 'msa_config':
      // 浏览器演示不需要 client_id，流程照样能走完；真机上这里由编译期常量决定
      return { configured: true, hint: null };

    case 'msa_start': {
      msaPolls = 0;
      msaPending = true;
      const now = Math.floor(Date.now() / 1000);
      return {
        userCode: 'K7Q2WP4XL',
        verificationUri: 'https://www.microsoft.com/link',
        expiresAt: now + 900,
        interval: 10,
        message: '若要登录，请使用 Web 浏览器打开页面 https://www.microsoft.com/link 并输入代码 K7Q2WP4XL 以进行身份验证。',
      };
    }

    case 'msa_poll': {
      if (!msaPending) throw new Error('还没有发起登录：先点「添加正版账号」');
      msaPolls += 1;
      // 前两次装成"用户还没在浏览器里点完"，第三次成功
      if (msaPolls < 3) {
        return { state: 'pending', interval: 10, expiresIn: 900 - msaPolls * 10 };
      }
      msaPending = false;
      const demo: Account =
        state.accounts.find((a) => a.kind === 'msa') ?? {
          id: 'msa-demo',
          kind: 'msa',
          name: 'SkyDy',
          uuid: '069a79f4-44e9-4726-a5be-fca90e38aaf5',
          skinModel: 'classic',
          skinUrl: null,
          lastUsed: nowIso(),
        };
      demo.lastUsed = nowIso();
      state.activeAccountId = demo.id;
      if (!state.accounts.some((a) => a.id === demo.id)) state.accounts.push(demo);
      msaTokens.add(demo.id);
      return { state: 'done', account: clone(demo), expiresIn: 86_400 };
    }

    case 'msa_cancel':
      msaPending = false;
      return null;

    case 'msa_states': {
      const now = Math.floor(Date.now() / 1000);
      return state.accounts
        .filter((a) => a.kind === 'msa')
        .map((a) => ({
          accountId: a.id,
          hasRefreshToken: msaTokens.has(a.id),
          expiresAt: msaTokens.has(a.id) ? now + 3600 : null,
          usable: msaTokens.has(a.id),
        }));
    }

    case 'plugins_list':
      return clone(state.plugins);

    case 'plugins_set_enabled': {
      const id = String(arg<string>(args, 'id'));
      const enabled = Boolean(arg<boolean>(args, 'enabled'));
      const plugin = state.plugins.find((p) => p.id === id);
      if (!plugin) throw new Error(`未找到插件：${id}`);
      plugin.enabled = enabled;
      plugin.state = enabled ? 'ready' : 'disabled';
      const list = clone(state.plugins);
      emit('plugins-changed', list);
      return list;
    }

    case 'launch_plan': {
      const root = requireRoot(String(arg<string>(args, 'rootId')));
      const versionId = String(arg<string>(args, 'versionId'));
      const summary = requireVersion(root.id, versionId);
      const effective = effectiveFor(root, versionId);
      const account = activeAccount();
      const isolated = effective.isolated;
      const gameDir = isolated ? `${root.path}\\versions\\${versionId}` : root.path;
      const javaPath =
        effective.javaPath ?? 'C:\\Program Files\\Eclipse Adoptium\\jdk-21.0.5+11-hotspot\\bin\\javaw.exe';
      const plan: LaunchPlan = {
        versionId,
        javaPath,
        javaSource: effective.javaPath ? 'explicit' : summary.javaMajor === null ? 'fallback' : 'auto',
        gameDir,
        assetsDir: `${root.path}\\assets`,
        librariesDir: `${root.path}\\libraries`,
        isolated,
        mainClass: launchMainClass(summary.loader),
        classpath: [
          `${root.path}\\libraries\\net\\fabricmc\\fabric-loader\\0.16.9\\fabric-loader-0.16.9.jar`,
          `${root.path}\\versions\\${versionId}\\${versionId}.jar`,
        ],
        jvmArgs: ['-Xmx4096M', '-XX:+UseG1GC', '-Dfile.encoding=UTF-8'],
        gameArgs: ['--username', account?.name ?? 'Player', '--version', versionId, '--gameDir', gameDir],
        extraJvmArgs: effective.jvmArgs,
        username: account?.name ?? 'Player',
        uuid: account?.uuid ?? '',
        assetIndex: summary.loader === 'vanilla' ? versionId : summary.inheritsFrom,
        launcherBrand: effective.launchBrand,
        launchDir: gameDir,
        nativesDir: `${gameDir}\\natives`,
        librariesDirs: [`${root.path}\\versions\\${versionId}\\libraries`, `${root.path}\\libraries`],
        gameVersion: summary.gameVersion ?? versionId,
        accessTokenInjected: false,
      };
      return plan;
    }

    /* ---- 本地内容 ---- */


    // ---- 模组元数据 / 图标 / 更新 ----
    case 'content_mod_icon': {
      const root = requireRoot(String(arg<string>(args, 'rootId')));
      const versionId = String(arg<string>(args, 'versionId'));
      const fileName = String(arg<string>(args, 'fileName'));
      const mod = contentFor(root.id, versionId).mods.find((m) => m.fileName === fileName);
      if (!mod?.hasIcon) throw new Error(`${fileName} 没有图标`);
      // 一张 1×1 的灰 PNG：浏览器里够用来验证「图标排在最前面」这条布局
      return {
        dataUrl:
          'data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==',
        mime: 'image/png',
      };
    }

    case 'mods_updates_check': {
      const root = requireRoot(String(arg<string>(args, 'rootId')));
      const versionId = String(arg<string>(args, 'versionId'));
      const mods = contentFor(root.id, versionId).mods.filter((m) => m.enabled);
      const updates = mods.slice(0, 2).map((mod, index) => ({
        fileName: mod.fileName,
        modId: mod.modId,
        source: 'modrinth',
        projectId: `mock-project-${index}`,
        currentVersion: mod.version,
        latestVersion: index === 0 ? '0.6.0' : '1.9.0',
        latestFileName: mod.fileName.replace(/-\d[\d.+]*/, index === 0 ? '-0.6.0' : '-1.9.0'),
        downloadUrl: 'https://example.invalid/mod.jar',
        size: mod.sizeBytes + 1024,
        sha1: null,
        pageUrl: 'https://example.invalid/project',
      }));
      return {
        updates,
        unknown: mods.slice(2, 4).map((m) => m.fileName),
        warnings: [],
        checkedAt: Date.now(),
      };
    }

    case 'mods_apply_update': {
      const root = requireRoot(String(arg<string>(args, 'rootId')));
      const versionId = String(arg<string>(args, 'versionId'));
      const update = arg<{ fileName: string; latestFileName: string; latestVersion: string }>(
        args,
        'update',
      );
      if (!update) throw new Error('没有要安装的更新');
      const store = contentFor(root.id, versionId);
      const index = store.mods.findIndex((m) => m.fileName === update.fileName);
      if (index < 0) throw new Error(`模组不存在：${update.fileName}`);
      const old = store.mods[index];
      store.mods[index] = {
        ...old,
        fileName: update.latestFileName,
        version: update.latestVersion,
        modifiedAt: Math.floor(Date.now() / 1000),
      };
      return null;
    }
    case 'content_dirs': {
      const root = requireRoot(String(arg<string>(args, 'rootId')));
      const versionId = String(arg<string>(args, 'versionId'));
      requireVersion(root.id, versionId);
      contentFor(root.id, versionId);
      return mockDirs(root, versionId);
    }

    case 'content_mods_list': {
      const root = requireRoot(String(arg<string>(args, 'rootId')));
      const versionId = String(arg<string>(args, 'versionId'));
      return clone(contentFor(root.id, versionId).mods);
    }

    case 'content_mods_set_enabled': {
      const root = requireRoot(String(arg<string>(args, 'rootId')));
      const versionId = String(arg<string>(args, 'versionId'));
      const file = String(arg<string>(args, 'file') ?? '');
      const enabled = Boolean(arg<boolean>(args, 'enabled'));
      const store = contentFor(root.id, versionId);
      const mod = store.mods.find((m) => m.fileName === file || m.fileName === `${file}.disabled`);
      if (!mod) throw new Error(`未找到模组：${file}`);
      mod.enabled = enabled;
      mod.fileName = enabled
        ? mod.fileName.replace(/\.disabled$/i, '')
        : mod.fileName.replace(/\.jar$/i, '.jar.disabled');
      return clone(store.mods);
    }

    case 'content_packs_list': {
      const root = requireRoot(String(arg<string>(args, 'rootId')));
      const versionId = String(arg<string>(args, 'versionId'));
      return clone(contentFor(root.id, versionId).packs);
    }

    case 'content_saves_list': {
      const root = requireRoot(String(arg<string>(args, 'rootId')));
      const versionId = String(arg<string>(args, 'versionId'));
      return clone(contentFor(root.id, versionId).saves);
    }

    case 'content_logs_list': {
      const root = requireRoot(String(arg<string>(args, 'rootId')));
      const versionId = String(arg<string>(args, 'versionId'));
      return clone(contentFor(root.id, versionId).logs);
    }

    case 'content_logs_read': {
      const root = requireRoot(String(arg<string>(args, 'rootId')));
      const versionId = String(arg<string>(args, 'versionId'));
      const file = String(arg<string>(args, 'file') ?? '');
      if (!/^[A-Za-z0-9._-]+$/.test(file)) throw new Error(`文件名不合法：${file}`);
      const store = contentFor(root.id, versionId);
      const meta = store.logs.find((l) => l.fileName === file);
      if (!meta) throw new Error(`未找到日志：${file}`);
      const text = store.logText[file] ?? '';
      return { fileName: file, totalBytes: meta.sizeBytes, shownBytes: text.length, truncated: meta.sizeBytes > text.length, text };
    }

    case 'content_import': {
      const root = requireRoot(String(arg<string>(args, 'rootId')));
      const versionId = String(arg<string>(args, 'versionId'));
      const kind = String(arg<string>(args, 'kind') ?? 'mod') as ContentKind;
      const paths = (arg<string[]>(args, 'paths') ?? []).map(String);
      const store = contentFor(root.id, versionId);
      const copied: string[] = [];
      const skipped: ImportSkipped[] = [];
      for (const raw of paths) {
        const name = raw.replace(/[\\/]+$/, '').split(/[\\/]/).pop() ?? raw;
        const exists =
          kind === 'mod'
            ? store.mods.some((m) => m.fileName === name || m.fileName === `${name}.disabled`)
            : store.packs.some((p) => p.fileName === name);
        if (exists) {
          skipped.push({ file: name, reason: '同名文件已存在（不覆盖）' });
          continue;
        }
        if (kind === 'mod') {
          store.mods.push({
            fileName: name,
            displayName: name.replace(/\.jar(\.disabled)?$/i, ''),
            version: null,
            sizeBytes: 1_048_576,
            enabled: true,
            modifiedAt: Math.floor(Date.now() / 1000),
            modId: null,
            name: null,
            authors: [],
            flavors: [],
            hasIcon: false,
            metaSource: 'filename',
          });
        } else {
          store.packs.push({
            fileName: name,
            displayName: name.replace(/\.zip$/i, ''),
            sizeBytes: 1_048_576,
            enabled: false,
            missing: false,
            isDir: false,
            modifiedAt: Math.floor(Date.now() / 1000),
          });
        }
        copied.push(name);
      }
      return { copied, skipped };
    }

    case 'content_delete': {
      const root = requireRoot(String(arg<string>(args, 'rootId')));
      const versionId = String(arg<string>(args, 'versionId'));
      const kind = String(arg<string>(args, 'kind') ?? 'mod') as ContentKind;
      const file = String(arg<string>(args, 'file') ?? '');
      const store = contentFor(root.id, versionId);
      if (kind === 'mod') {
        const index = store.mods.findIndex((m) => m.fileName === file);
        if (index < 0) throw new Error(`未找到模组：${file}`);
        store.mods.splice(index, 1);
      } else {
        const index = store.packs.findIndex((p) => p.fileName === file);
        if (index < 0) throw new Error(`未找到资源包：${file}`);
        store.packs.splice(index, 1);
      }
      return null;
    }

    /* ---- Java 运行时 ---- */

    case 'java_list':
      return mockJavaList();

    case 'java_inspect': {
      const path = String(arg<string>(args, 'path') ?? '');
      if (!path) throw new Error('路径为空');
      const home = path.replace(/[\\/]bin[\\/](java|javaw)\.exe$/i, '').replace(/[\\/](java|javaw)\.exe$/i, '');
      const known = mockJavaList().find((r) => r.home === home);
      if (known) return known;
      return {
        path: `${home}\\bin\\javaw.exe`,
        home,
        name: `自定义 ${home.split(/[\\/]/).pop() ?? 'Java'}`,
        version: '21.0.5',
        major: 21,
        vendor: '自定义',
        arch: 'x86_64',
        source: 'custom' as const,
        valid: true,
      };
    }

    case 'java_pick': {
      const explicit = arg<string | null>(args, 'javaPath');
      if (explicit) {
        return (
          mockJavaList().find((r) => r.path.toLowerCase() === explicit.toLowerCase()) ?? {
            path: explicit,
            home: explicit.replace(/[\\/]bin[\\/].*$/i, ''),
            name: '手动指定',
            version: '21.0.5',
            major: 21,
            vendor: '自定义',
            arch: 'x86_64',
            source: 'settings' as const,
            valid: true,
          }
        );
      }
      return mockPickJava(arg<number | null>(args, 'major') ?? null);
    }

    case 'versions_verify': {
      const root = requireRoot(String(arg<string>(args, 'rootId')));
      const versionId = String(arg<string>(args, 'versionId'));
      const summary = requireVersion(root.id, versionId);
      const incomplete =
        typeof summary.readiness === 'object' ? summary.readiness.incomplete.missing : 0;
      return {
        versionId,
        ok: incomplete === 0,
        missingJar: false,
        missingLibraries: incomplete > 2 ? incomplete - 2 : 0,
        missingAssetsIndex: false,
        missingAssetsObjects: incomplete > 2 ? 2 : incomplete,
        assetsObjectTotal: 2143,
        missingTotal: incomplete,
      };
    }

    /* ---- 模组下载 ---- */

    case 'mods_types':
      return clone(MOCK_MOD_TYPES);

    case 'mods_target': {
      const root = requireRoot(String(arg<string>(args, 'rootId')));
      const versionId = String(arg<string>(args, 'versionId'));
      const summary = requireVersion(root.id, versionId);
      const isolated = resolveIsolation(root);
      // 宿主会一路走 inheritsFrom 到原版；这里近似成「取 id 里的版本号前缀」。
      const gameVersion =
        summary.inheritsFrom ?? /^\d+\.\d+(?:\.\d+)?/.exec(summary.id)?.[0] ?? summary.id;
      return {
        rootId: root.id,
        versionId,
        gameVersion,
        loader: summary.loader,
        loaderLabel: MOCK_LOADER_LABELS[summary.loader] ?? summary.loader,
        modsDir: `${isolated ? `${root.path}\\versions\\${versionId}` : root.path}\\mods`,
        isolated,
        warning:
          summary.loader === 'vanilla'
            ? '当前版本是原版，装上的模组不会被加载；先装一个 Fabric / Forge 版本再下载模组。'
            : null,
      };
    }

    case 'mods_ranking':
      // 真机上排行榜就是"无条件搜索 = 按下载量"（PCL2 的空条件搜索结果）；
      // 演示里复用同一套过滤，类型筛选与翻页也一起生效。
      return mockModSearch(args);

    case 'mods_search':
      return mockModSearch(args);

    case 'mods_detail': {
      const source = String(arg<string>(args, 'source') ?? 'modrinth') as ModSource;
      const projectId = String(arg<string>(args, 'projectId'));
      return mockModDetail(source, projectId);
    }

    case 'mods_install': {
      const fileId = String(arg<string>(args, 'fileId') ?? '');
      if (!fileId) throw new Error('缺少文件 id');
      const jobId = `mock-mod-${Date.now()}`;
      emit('downloads-changed', { jobId });
      return { jobId };
    }

    case 'plugins_slots':
      // 演示模式没有安装第三方清单，面板区直接空着。
      return [];
    case 'plugins_manifests':
      return [];

    default:
      throw new Error(`未知命令：${cmd}`);
  }
}

/* ------------------------------------------------------------------ *
 * Bridge implementation
 * ------------------------------------------------------------------ */

export const mockBridge = {
  async invoke<T>(cmd: string, args?: unknown): Promise<T> {
    await delay(18);
    return (await dispatch(cmd, args)) as T;
  },
  async listen(event: string, cb: (payload: unknown) => void): Promise<() => void> {
    const set = listeners.get(event) ?? new Set<(payload: unknown) => void>();
    set.add(cb);
    listeners.set(event, set);
    return () => {
      set.delete(cb);
    };
  },
  /** 浏览器演示里没有外壳可调用，直接用新标签页打开。 */
  async openExternal(url: string): Promise<void> {
    if (typeof window === 'undefined') throw new Error('没有可用的浏览器窗口');
    window.open(url, '_blank', 'noopener,noreferrer');
  },
  /** 浏览器里没有标题栏可换，什么都不做（界面颜色由 CSS 变量负责）。 */
  async setTheme(_theme: 'dark' | 'light'): Promise<void> {},
  /**
   * 浏览器里没有窗口可最小化/隐藏，只把动作广播出去，
   * 界面侧照样能验证「启动后动作」这条分支跑没跑到。
   */
  async applyAfterLaunch(action: 'minimize' | 'hide_to_tray' | 'exit' | 'restore'): Promise<void> {
    emit('after-launch-action', { action });
  },
  /**
   * 浏览器里没有系统目录选择器，直接给一个假的桌面路径。
   *
   * 不能返回 `null`——那表示"用户取消了"，界面上导排障包就永远没有结果，
   * 演示时看不出这条流程通不通。
   */
  async pickFolder(): Promise<string | null> {
    return 'C:\\Users\\demo\\Desktop';
  },
};
