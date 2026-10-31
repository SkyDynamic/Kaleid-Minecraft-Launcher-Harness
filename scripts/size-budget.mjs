#!/usr/bin/env node
// 体积预算闸门。
//
// 用法：
//   node scripts/size-budget.mjs            # 检查 ui/dist 与（若存在）安装包
//   node scripts/size-budget.mjs --json     # 机器可读输出
//
// 预算来自 docs/superpowers/specs/2026-10-29-kaleid-launcher-design.md。
// 超标只报警（exit 0）还是失败（exit 1）由 --strict 控制；CI 用 --strict。

import { readdir, readFile, stat } from "node:fs/promises";
import { existsSync } from "node:fs";
import { gzipSync } from "node:zlib";
import { join, relative, extname } from "node:path";

const MB = 1024 * 1024;
const KB = 1024;

/** 目标值与闸门值。 */
const BUDGETS = {
  installer: { target: 8 * MB, gate: 12 * MB, label: "安装包（NSIS）" },
  mainChunk: { target: 220 * KB, gate: 300 * KB, label: "主界面 chunk（gzip）" },
  pluginChunk: { target: 60 * KB, gate: 90 * KB, label: "界面插件 chunk（gzip）" },
};

const args = new Set(process.argv.slice(2));
const strict = args.has("--strict");
const asJson = args.has("--json");
const root = process.cwd();

const results = [];

function record(name, actual, budget, note) {
  const status =
    actual == null
      ? "skip"
      : actual > budget.gate
        ? "fail"
        : actual > budget.target
          ? "warn"
          : "ok";
  results.push({
    name,
    label: budget.label,
    actual,
    target: budget.target,
    gate: budget.gate,
    status,
    note,
  });
}

async function walk(dir) {
  const out = [];
  for (const entry of await readdir(dir, { withFileTypes: true })) {
    const full = join(dir, entry.name);
    if (entry.isDirectory()) out.push(...(await walk(full)));
    else out.push(full);
  }
  return out;
}

// ---- 1. 界面产物 ----
const dist = join(root, "ui", "dist");
const pluginDist = join(root, "ui-plugins");

if (existsSync(dist)) {
  const files = await walk(dist);
  const js = files.filter((f) => extname(f) === ".js");
  let mainGzip = 0;
  let cssGzip = 0;
  const chunks = [];

  for (const file of js) {
    const buf = await readFile(file);
    const gz = gzipSync(buf, { level: 9 }).length;
    chunks.push({ file: relative(root, file), raw: buf.length, gzip: gz });
    // 主入口是 index-*.js；插件 chunk 由 ui-plugins 单独算
    if (/[\\/]index-[\w-]+\.js$/.test(file) || /[\\/]index\.js$/.test(file)) {
      mainGzip += gz;
    }
  }
  for (const file of files.filter((f) => extname(f) === ".css")) {
    cssGzip += gzipSync(await readFile(file), { level: 9 }).length;
  }

  record("mainChunk", mainGzip + cssGzip, BUDGETS.mainChunk, `${chunks.length} 个 JS 产物`);
  if (!asJson) {
    console.log("界面产物：");
    for (const c of chunks.sort((a, b) => b.gzip - a.gzip).slice(0, 8)) {
      console.log(
        `  ${c.file.padEnd(42)} ${(c.raw / KB).toFixed(1).padStart(7)} KB → ${(c.gzip / KB).toFixed(1).padStart(6)} KB gzip`,
      );
    }
  }
} else {
  record("mainChunk", null, BUDGETS.mainChunk, "ui/dist 不存在，先 pnpm --dir ui build");
}

// ---- 2. 界面插件产物（每个插件单独一个 chunk） ----
let worstPlugin = null;
if (existsSync(pluginDist)) {
  for (const dirent of await readdir(pluginDist, { withFileTypes: true })) {
    if (!dirent.isDirectory()) continue;
    const outDir = join(pluginDist, dirent.name, "dist");
    if (!existsSync(outDir)) continue;
    for (const file of (await walk(outDir)).filter((f) => extname(f) === ".js")) {
      const gz = gzipSync(await readFile(file), { level: 9 }).length;
      if (!worstPlugin || gz > worstPlugin.gzip) {
        worstPlugin = { name: dirent.name, file: relative(root, file), gzip: gz };
      }
    }
  }
}
record(
  "pluginChunk",
  worstPlugin ? worstPlugin.gzip : null,
  BUDGETS.pluginChunk,
  worstPlugin ? `最大：${worstPlugin.name}` : "ui-plugins/*/dist 不存在",
);

// ---- 3. 安装包 ----
const installerDirs = [
  join(root, "src-tauri", "target", "release", "bundle", "nsis"),
  join(root, "dist"),
];
let installer = null;
for (const dir of installerDirs) {
  if (!existsSync(dir)) continue;
  for (const file of await walk(dir)) {
    if (extname(file) !== ".exe") continue;
    const size = (await stat(file)).size;
    if (!installer || size > installer.size) installer = { file, size };
  }
}
record(
  "installer",
  installer ? installer.size : null,
  BUDGETS.installer,
  installer ? relative(root, installer.file) : "还没有打包，跳过",
);

// ---- 输出 ----
const fmt = (n) => (n == null ? "—" : n >= MB ? `${(n / MB).toFixed(2)} MB` : `${(n / KB).toFixed(1)} KB`);
const icon = { ok: "✅", warn: "⚠️ ", fail: "❌", skip: "⏭️ " };

if (asJson) {
  console.log(JSON.stringify(results, null, 2));
} else {
  console.log("\n体积预算：");
  for (const r of results) {
    console.log(
      `  ${icon[r.status]} ${r.label.padEnd(22)} ${fmt(r.actual).padStart(10)}  ` +
        `(目标 ${fmt(r.target)} / 闸门 ${fmt(r.gate)})  ${r.note}`,
    );
  }
}

const failed = results.filter((r) => r.status === "fail");
if (failed.length && strict) {
  console.error(`\n${failed.length} 项超出闸门值。`);
  process.exit(1);
}
