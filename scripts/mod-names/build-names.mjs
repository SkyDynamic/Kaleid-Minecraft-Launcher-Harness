#!/usr/bin/env node
/**
 * 扩充模组中文名词表（开发期离线脚本，运行时零抓取）。
 *
 * 背景：MCIM 的翻译接口只译**简介**，不译名称；模组列表里的中文名靠启动器自带
 * 的词表（PCL2 也是这么做的）。词表本身随包分发，见
 * `crates/kaleid-core-plugins/assets/mod-names-zh.seed.json`。
 *
 * 这个脚本负责把词表扩到热门模组上：
 *
 * 1. 从 MCIM 镜像取 Modrinth / CurseForge 的下载量排行，得到候选 (英文名, slug)；
 * 2. 用英文名去 MC 百科搜索，取第一条 `class/<id>.html` 结果；
 * 3. **只接受英文名归一化后完全相等的结果**（结果标题形如 `钠 ( Sodium )`），
 *    否则宁可不收录——错译名比没有译名更糟；
 * 4. 合并进种子词表（保留已有条目，按英文名排序）。
 *
 * 用法：
 *   node scripts/mod-names/build-names.mjs                 # 默认每源 60 个候选
 *   node scripts/mod-names/build-names.mjs --limit 150     # 扩到 150
 *   node scripts/mod-names/build-names.mjs --dry-run       # 只打印不写盘
 *
 * 注意：MC 百科有速率限制，脚本串行 + 1.2s 间隔，几百个候选要跑几分钟。
 */

import { readFile, writeFile } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import path from 'node:path';

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..');
const SEED = path.join(ROOT, 'crates/kaleid-core-plugins/assets/mod-names-zh.seed.json');

const MCIM = 'https://mod.mcimirror.top';
const MCMOD_SEARCH = 'https://search.mcmod.cn/s';
/** 启动器自己申请的 UA 白名单前缀，见 build.env 的 KMLH_VERSION_CODE。 */
const MCIM_UA = `KMLH/${process.env.KMLH_VERSION_CODE ?? '0.1.0'}`;
const MCMOD_UA =
  'Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0 Safari/537.36';
const DELAY_MS = 1200;

const args = new Map();
for (let i = 2; i < process.argv.length; i += 1) {
  const arg = process.argv[i];
  if (!arg.startsWith('--')) continue;
  const body = arg.slice(2);
  const eq = body.indexOf('=');
  if (eq >= 0) {
    args.set(body.slice(0, eq), body.slice(eq + 1));
  } else if (process.argv[i + 1] && !process.argv[i + 1].startsWith('--')) {
    // 支持 `--limit 60` 这种空格写法，否则 Number('true') 会静默变成 NaN。
    args.set(body, process.argv[i + 1]);
    i += 1;
  } else {
    args.set(body, 'true');
  }
}
const LIMIT = Number(args.get('limit') ?? 60);
if (!Number.isFinite(LIMIT) || LIMIT <= 0) {
  console.error(`--limit 需要正数，收到：${args.get('limit')}`);
  process.exit(1);
}
const DRY_RUN = args.has('dry-run');

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

/** 与 Rust 侧 `normalize_name` 保持一致的近似实现：只留字母数字。 */
function normalize(value) {
  return String(value ?? '')
    .toLowerCase()
    .replace(/[^a-z0-9]/g, '');
}

/**
 * 站点标题里常带后缀，例如 `Just Enough Items (JEI)`；MC 百科的条目名只有
 * `Just Enough Items`。查询和比对都用剥掉括号后缀的名字，否则永远匹配不上。
 */
function stemName(name) {
  return String(name ?? '')
    .replace(/[（(][^)）]*[)）]\s*$/, '')
    .trim();
}

/** MC 百科条目标题会带短名标签，例如 `[FC] 铁氧体磁芯`，显示时去掉。 */
function cleanZh(zh) {
  return String(zh ?? '')
    .replace(/^\s*\[[^\]]{1,12}\]\s*/, '')
    .trim();
}

async function getJson(url, headers = {}) {
  const response = await fetch(url, {
    headers: { 'user-agent': MCIM_UA, accept: 'application/json', ...headers },
    signal: AbortSignal.timeout(30_000),
  });
  if (!response.ok) throw new Error(`${response.status} ${url}`);
  return response.json();
}

/** Modrinth 下载量排行（镜像）。 */
async function modrinthCandidates(limit) {
  const out = [];
  for (let offset = 0; out.length < limit && offset < 400; offset += 100) {
    const facets = encodeURIComponent('[["project_type:mod"]]');
    const page = await getJson(
      `${MCIM}/modrinth/v2/search?limit=100&offset=${offset}&index=downloads&facets=${facets}`,
    );
    for (const hit of page.hits ?? []) {
      out.push({ name: hit.title, slug: hit.slug });
    }
    if ((page.hits ?? []).length < 100) break;
    await sleep(DELAY_MS);
  }
  return out.slice(0, limit);
}

/** CurseForge 下载量排行（镜像，mods 分类 = classId 6）。 */
async function curseforgeCandidates(limit) {
  const out = [];
  const pageSize = 50;
  for (let index = 0; out.length < limit && index < 400; index += pageSize) {
    const page = await getJson(
      `${MCIM}/curseforge/v1/mods/search?gameId=432&classId=6&sortField=6&sortOrder=desc&pageSize=${pageSize}&index=${index}`,
    );
    for (const item of page.data ?? []) {
      out.push({ name: item.name, slug: item.slug });
    }
    if ((page.data ?? []).length < pageSize) break;
    await sleep(DELAY_MS);
  }
  return out.slice(0, limit);
}

/**
 * 在 MC 百科搜一个模组。
 *
 * 返回 `{ zh, en, mcmodId }`；只有英文名归一化完全相等才算命中。
 * 搜索结果标题多数是 `中文名 ( English )`；少数只有中文，这时再读一次条目页
 * 的 `<title>中文 ( English ) - MC百科</title>` 来核对——两次请求都验证不过就放弃。
 */
async function searchMcmod(name) {
  const query = stemName(name) || name;
  const url = `${MCMOD_SEARCH}?key=${encodeURIComponent(query)}&site=all&filter=0`;
  const response = await fetch(url, {
    headers: { 'user-agent': MCMOD_UA, 'accept-language': 'zh-CN,zh;q=0.9' },
    signal: AbortSignal.timeout(30_000),
  });
  if (!response.ok) throw new Error(`${response.status} mcmod`);
  const html = await response.text();

  const anchor = /href="(?:https:\/\/www\.mcmod\.cn)?\/class\/(\d+)\.html"[^>]*>([\s\S]{0,200}?)<\/a>/.exec(
    html,
  );
  if (!anchor) return null;

  const mcmodId = Number(anchor[1]);
  const text = anchor[2]
    .replace(/<[^>]+>/g, ' ')
    .replace(/\s+/g, ' ')
    .trim();
  const matched = /^(.*?)\s*[（(]\s*(.*?)\s*[)）]\s*$/.exec(text);

  if (matched && normalize(matched[2]) === normalize(query)) {
    const zh = cleanZh(matched[1]);
    return zh ? { zh, en: stemName(name) || name, mcmodId } : null;
  }

  // 搜索结果没带英文名：读条目页标题核对，避免把同名不同模组收进来。
  const page = await fetch(`https://www.mcmod.cn/class/${mcmodId}.html`, {
    headers: { 'user-agent': MCMOD_UA, 'accept-language': 'zh-CN,zh;q=0.9' },
    signal: AbortSignal.timeout(30_000),
  });
  if (!page.ok) return null;
  const title = /<title>\s*([\s\S]{0,160}?)<\/title>/.exec(await page.text());
  if (!title) return null;
  const parts = /^(.*?)\s*[（(]\s*(.*?)\s*[)）]/.exec(title[1].trim());
  if (!parts) return null;
  if (normalize(parts[2]) !== normalize(query)) return null;
  const zh = cleanZh(parts[1]);
  return zh ? { zh, en: stemName(name) || name, mcmodId } : null;
}

function mergeEntries(existing, incoming) {
  const byName = new Map();
  for (const entry of existing) byName.set(normalize(entry.en) || `slug:${entry.slug}`, entry);
  let added = 0;
  let verified = 0;
  for (const entry of incoming) {
    const key = normalize(entry.en) || `slug:${entry.slug}`;
    const current = byName.get(key);
    if (!current) {
      byName.set(key, entry);
      added += 1;
      continue;
    }
    // 没有 mcmodId 的条目是人工随手写的，应该被「核对过条目页」的结果纠正；
    // 已经有 mcmodId 的条目视为已校对，只补空洞字段。
    if (!current.mcmodId && entry.mcmodId) {
      byName.set(key, { ...entry, slug: current.slug ?? entry.slug });
      verified += 1;
      continue;
    }
    let touched = false;
    if (!current.slug && entry.slug) {
      current.slug = entry.slug;
      touched = true;
    }
    if (touched) byName.set(key, current);
  }
  const entries = [...byName.values()].sort((a, b) =>
    (a.en || a.zh).localeCompare(b.en || b.zh, 'en'),
  );
  return { entries, added, verified };
}

async function main() {
  const library = JSON.parse(await readFile(SEED, 'utf8'));
  const existing = library.entries ?? [];
  console.log(`现有词表：${existing.length} 条`);

  const [modrinth, curseforge] = await Promise.all([
    modrinthCandidates(LIMIT).catch((error) => {
      console.warn(`Modrinth 排行获取失败：${error.message}`);
      return [];
    }),
    curseforgeCandidates(LIMIT).catch((error) => {
      console.warn(`CurseForge 排行获取失败：${error.message}`);
      return [];
    }),
  ]);
  console.log(`候选：Modrinth ${modrinth.length} · CurseForge ${curseforge.length}`);

  const seen = new Set();
  const candidates = [];
  for (const item of [...modrinth, ...curseforge]) {
    const key = normalize(item.name);
    if (!key || seen.has(key)) continue;
    seen.add(key);
    candidates.push(item);
  }
  console.log(`去重后待查：${candidates.length} 个（串行 ${DELAY_MS}ms，约 ${Math.ceil((candidates.length * DELAY_MS) / 1000)}s）`);

  const found = [];
  let index = 0;
  for (const candidate of candidates) {
    index += 1;
    try {
      const hit = await searchMcmod(candidate.name);
      if (hit) {
        found.push({ zh: hit.zh, en: hit.en, slug: candidate.slug, mcmodId: hit.mcmodId });
        console.log(`  [${index}/${candidates.length}] ${candidate.name} → ${hit.zh} (#${hit.mcmodId})`);
      } else {
        console.log(`  [${index}/${candidates.length}] ${candidate.name} → 未命中，跳过`);
      }
    } catch (error) {
      console.warn(`  [${index}/${candidates.length}] ${candidate.name} → 失败：${error.message}`);
    }
    await sleep(DELAY_MS);
  }

  const { entries, added, verified } = mergeEntries(existing, found);
  console.log(
    `\n命中 ${found.length} 个，新增 ${added} 条，纠正 ${verified} 条，词表共 ${entries.length} 条`,
  );

  if (DRY_RUN) {
    console.log('--dry-run：不写盘');
    return;
  }
  library.entries = entries;
  await writeFile(SEED, `${JSON.stringify(library, null, 2)}\n`, 'utf8');
  console.log(`已写入 ${path.relative(ROOT, SEED)}`);
}

main().catch((error) => {
  console.error(error);
  process.exit(1);
});
