/**
 * 日志着色解析的自检：`ui/src/lib/logFormat.ts` 是纯函数，直接在 Node 里跑它。
 *
 *   node scripts/log-format-check.mjs
 *
 * 界面层没有单测跑器（没有 vitest），但"级别认没认出来、ANSI 有没有被当成乱码、
 * 拼回去是不是原文"这三件事是最容易在改动中悄悄坏掉的，所以单独留一个可执行检查。
 * 退出码非 0 表示有用例失败。
 */
import { parseLog, parseLogLine } from '../ui/src/lib/logFormat.ts';

let failed = 0;

function check(name, condition, detail) {
  if (condition) {
    console.log(`  ok  ${name}`);
    return;
  }
  failed += 1;
  console.error(`  FAIL ${name}${detail ? ` — ${detail}` : ''}`);
}

function levelOf(line) {
  return parseLogLine(line).level;
}

console.log('级别识别');
check('log4j 布局 INFO', levelOf('[12:00:00] [main/INFO]: hi') === 'info');
check('带空格 + logger 名', levelOf('[12:00:00] [main/INFO] [a.b.C/]: hi') === 'info');
check('线程名含空格', levelOf('[12:00:05] [Render thread/ERROR] [x/]: hi') === 'error');
check('裸级别行', levelOf('[WARN] hi') === 'warn');
check('时间戳 + 裸级别', levelOf('[12:00:00] [FATAL] hi') === 'fatal');
check('无级别的正文', levelOf('随便一行没有级别') === 'plain');

console.log('前缀不再当成正文');
const parsed = parseLogLine('[12:00:00] [main/INFO] [ne.neoforge.fml/]: Found mod file');
const textOf = (tone) => parsed.spans.filter((s) => s.tone === tone).map((s) => s.text).join('');
check('时间戳单独成段', textOf('time').includes('[12:00:00]'));
check('级别单独成段', textOf('lv-info') === 'INFO');
check('logger 名淡显', textOf('thread').includes('[ne.neoforge.fml/]'));
check('正文只剩消息', parsed.spans[parsed.spans.length - 1].text === 'Found mod file');

console.log('堆栈续行');
check('at 帧', parseLogLine('\tat a.b.C(D.java:1)').continuation);
check('Caused by', parseLogLine('Caused by: java.lang.NullPointerException').continuation);
check('... n more', parseLogLine('\t... 12 more').continuation);

console.log('ANSI');
const ansi = parseLogLine('\u001b[32mG\u001b[0m \u001b[1;31mR\u001b[0m').spans;
check('绿色分段', ansi.some((s) => s.tone === 'ansi-green' && s.text === 'G'));
check('红色分段且加粗', ansi.some((s) => s.tone === 'ansi-red' && s.text === 'R' && s.strong));
check('非 SGR 转义被丢掉', !parseLogLine('a\u001b[2Kb').spans.map((s) => s.text).join('').includes('\u001b'));

console.log('拼回去等于原文（只差 ANSI 控制码）');
const samples = [
  '[12:00:00] [main/INFO] [a/b/]: hello world',
  '[12:00:01] [Worker-Main-3/DEBUG] [chunk/]: Chunk [3, -7] loaded in 18ms',
  '  前导空格保留',
  '\tat a.b.C(D.java:1)',
];
for (const sample of samples) {
  const joined = parseLogLine(sample).spans.map((s) => s.text).join('');
  check(`原文可还原：${sample.slice(0, 24)}…`, joined === sample, JSON.stringify(joined));
}

console.log('统计与截断');
const body = [
  '[12:00:00] [main/INFO]: a',
  '[12:00:01] [main/WARN]: b',
  '[12:00:02] [main/ERROR]: c',
  '\tat x',
  '[12:00:03] [main/FATAL]: d',
  '',
].join('\n');
const log = parseLog(body);
check('WARN 计数', log.counts.warn === 1, String(log.counts.warn));
check('ERROR + FATAL 都算错误', log.counts.error === 2, String(log.counts.error));
check('续行不计入统计', log.totalLines === 5, String(log.totalLines));
const capped = parseLog(body, 2);
check('超上限只留尾部', capped.lines.length === 2 && capped.omitted === 3, `${capped.lines.length}/${capped.omitted}`);
check('统计仍按全文', capped.counts.error === 2);

console.log(failed === 0 ? '\n全部通过' : `\n${failed} 条失败`);
process.exit(failed === 0 ? 0 : 1);
