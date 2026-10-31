/**
 * 时间格式化的自检：`ui/src/lib/util.ts` 里的 `formatDate` / `formatDateTime` 是纯函数，
 * 直接在 Node 里跑（Node 22+ 自带 TS 类型擦除）。
 *
 *   node scripts/time-format-check.mjs
 *
 * 起因是一个真 bug：存档的 `LastPlayed` 在 NBT 里是**毫秒**，界面按**秒**解释，
 * 于是「最后游玩」显示成 5xxxx 年。现在后端在边界上换算成秒，格式化函数再用
 * 1e12 分界兜底——两头都留了断言，谁改坏了都跑不出来。
 */
import { formatDate, formatDateTime } from '../ui/src/lib/util.ts';

let failed = 0;

function check(name, actual, expected) {
  if (actual === expected) {
    console.log(`  ok  ${name}`);
    return;
  }
  failed += 1;
  console.error(`  FAIL ${name} — 期望 ${expected}，实际 ${actual}`);
}

/** 只比较年份：本地时区不同，具体日期可能差一天，年份不该差。 */
function yearOf(text) {
  return text.slice(0, 4);
}

// 1761509640 = 2025-10-26T20:14:00Z；毫秒版与 ISO 版应当渲染成一模一样。
const seconds = 1_761_509_640;
const millis = 1_761_509_640_000;
const iso = new Date(seconds * 1000).toISOString();

console.log('秒 / 毫秒 / ISO 串三种写法');
check('按秒解释的年份', yearOf(formatDateTime(seconds)), '2025');
check('毫秒与秒结果一致', formatDateTime(millis), formatDateTime(seconds));
check('ISO 串与秒结果一致', formatDateTime(iso), formatDateTime(seconds));
check('formatDate 也认毫秒', yearOf(formatDate(millis)), '2025');

console.log('分界不误伤');
check('2100 年的秒（4102444800）', yearOf(formatDate(4_102_444_800)), '2100');
check('毫秒与对应秒同一天', formatDate(4_102_444_800_000), formatDate(4_102_444_800));

console.log('空值与垃圾输入');
check('null', formatDateTime(null), '—');
check('undefined', formatDateTime(undefined), '—');
check('空串', formatDate(''), '—');
check('NaN', formatDateTime(Number.NaN), '—');
check('认不出来的字符串原样返回', formatDateTime('不是时间'), '不是时间');

console.log('存档那个真 bug 不会再出现');
const save = formatDateTime(seconds);
check('不会出现 5 位数年份', String(/^\d{5}/.test(save)), 'false');
check('格式是 YYYY/MM/DD HH:mm', String(/^\d{4}\/\d{2}\/\d{2} \d{2}:\d{2}$/.test(save)), 'true');

console.log(failed === 0 ? '\n全部通过' : `\n${failed} 条失败`);
process.exit(failed === 0 ? 0 : 1);
