/**
 * 校验词表里的 slug 到底指向哪个模组（离线维护工具）。
 *
 *   node scripts/mod-names/verify-slugs.mjs
 *
 * 只做两件事：slug 在 Modrinth / CurseForge（经 MCIM 镜像）里真实存在，且站上
 * 标题与词表里的 `en` 对得上。对不上就**丢掉 slug**（词表仍可靠英文名命中）、
 * 标题更准确时顺手订正 `en`，并把改动打印出来供人工复核。
 */
import { readFile, writeFile } from 'node:fs/promises';
import path from 'node:path';

const MCIM = 'https://mod.mcimirror.top';
const SEED = 'crates/kaleid-core-plugins/assets/mod-names-zh.seed.json';
const UA = 'KMLH/0.1.0';
const norm = (v) =>
  String(v ?? '')
    .toLowerCase()
    .replace(/[^a-z0-9]/g, '');

async function json(url) {
  const response = await fetch(url, { headers: { 'user-agent': UA, accept: 'application/json' } });
  if (!response.ok) return null;
  try {
    return await response.json();
  } catch {
    return null;
  }
}

function alike(a, b) {
  const x = norm(a);
  const y = norm(b);
  if (!x || !y) return false;
  return x === y || x.includes(y) || y.includes(x);
}

const library = JSON.parse(await readFile(SEED, 'utf8'));
const report = { ok: 0, renamed: [], dropped: [] };

for (const entry of library.entries) {
  if (!entry.slug) {
    report.ok += 1;
    continue;
  }
  const project = await json(`${MCIM}/modrinth/v2/project/${entry.slug}`);
  if (project?.title) {
    if (alike(project.title, entry.en)) {
      if (norm(project.title) !== norm(entry.en)) report.renamed.push([entry.en, project.title, 'modrinth']);
      entry.en = project.title;
      report.ok += 1;
    } else {
      report.dropped.push([entry.en, entry.slug, `modrinth:${project.title}`]);
      delete entry.slug;
    }
    continue;
  }
  const cf = await json(`${MCIM}/curseforge/v1/mods/search?gameId=432&slug=${encodeURIComponent(entry.slug)}&pageSize=1`);
  const item = cf?.data?.[0];
  if (item?.name) {
    if (alike(item.name, entry.en)) {
      if (norm(item.name) !== norm(entry.en)) report.renamed.push([entry.en, item.name, 'curseforge']);
      entry.en = item.name;
      report.ok += 1;
    } else {
      report.dropped.push([entry.en, entry.slug, `curseforge:${item.name}`]);
      delete entry.slug;
    }
    continue;
  }
  report.dropped.push([entry.en, entry.slug, 'not-found']);
  delete entry.slug;
}

console.log(`slug 校验通过 ${report.ok} 条`);
if (report.renamed.length) {
  console.log('\n按站上真实标题订正 en：');
  for (const [from, to, site] of report.renamed) console.log(`  ${from} → ${to}  (${site})`);
}
if (report.dropped.length) {
  console.log('\n丢掉 slug（改名后会靠英文名命中）：');
  for (const [en, slug, why] of report.dropped) console.log(`  ${en}  [${slug}]  ${why}`);
}

library.entries.sort((a, b) => (a.en || a.zh).localeCompare(b.en || b.zh, 'en'));
await writeFile(SEED, `${JSON.stringify(library, null, 2)}\n`, 'utf8');
console.log(`\n已写回 ${path.basename(SEED)}`);
