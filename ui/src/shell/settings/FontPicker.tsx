import { useEffect, useMemo, useRef, useState } from 'react';
import { Check, ChevronDown, Loader2, Search, Upload } from 'lucide-react';
import { getBridge } from '../../bridge';
import { Input } from '../../primitives';
import { Popover } from '../../components/Menu';
import { usePrefs, type FontPref } from '../../store/prefs';
import { useUI } from '../../store/ui';
import { cx } from '../../lib/util';

const FALLBACK_FONTS = [
  'Microsoft YaHei UI',
  'Microsoft YaHei',
  'Segoe UI Variable Text',
  'Segoe UI',
  'Arial',
  'Consolas',
  'Cascadia Mono',
];

function systemUiFamily(fonts: string[]): string {
  const wanted = ['Microsoft YaHei UI', 'Microsoft YaHei', 'Segoe UI Variable Text', 'Segoe UI'];
  return wanted.find((name) => fonts.includes(name)) ?? fonts[0] ?? '系统字体';
}

function fontLabel(font: FontPref, systemName: string): string {
  if (font.mode === 'system') return `跟随系统（${systemName}）`;
  if (font.mode === 'imported') return `${font.family}（导入）`;
  return font.family;
}

export function FontPicker() {
  const font = usePrefs((s) => s.font);
  const setFont = usePrefs((s) => s.setFont);
  const showToast = useUI((s) => s.showToast);
  const [open, setOpen] = useState<{ x: number; y: number } | null>(null);
  const [query, setQuery] = useState('');
  const [fonts, setFonts] = useState<string[]>([]);
  const [loading, setLoading] = useState(false);
  const [importing, setImporting] = useState(false);
  const triggerRef = useRef<HTMLButtonElement>(null);

  useEffect(() => {
    let cancelled = false;
    const bridge = getBridge();
    if (!bridge.listFonts) {
      setFonts(FALLBACK_FONTS);
      return;
    }
    setLoading(true);
    bridge
      .listFonts()
      .then((list) => {
        if (!cancelled) setFonts(list.length > 0 ? list : FALLBACK_FONTS);
      })
      .catch(() => {
        if (!cancelled) setFonts(FALLBACK_FONTS);
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, []);

  const systemName = useMemo(() => systemUiFamily(fonts.length > 0 ? fonts : FALLBACK_FONTS), [fonts]);
  const filtered = useMemo(() => {
    const needle = query.trim().toLowerCase();
    return fonts.filter((name) => !needle || name.toLowerCase().includes(needle));
  }, [fonts, query]);

  const openPicker = () => {
    const rect = triggerRef.current?.getBoundingClientRect();
    if (!rect) return;
    setQuery('');
    setOpen({ x: rect.left, y: rect.bottom + 4 });
  };

  const importFont = async () => {
    const bridge = getBridge();
    if (!bridge.pickFont || !bridge.readFont) {
      showToast('当前环境不支持导入字体文件');
      return;
    }
    setImporting(true);
    try {
      const path = await bridge.pickFont();
      if (!path) return;
      const data = await bridge.readFont(path);
      const family = `Kaleid Imported ${data.name || 'Font'}`.replace(/["\\]/g, '');
      setFont({
        mode: 'imported',
        family,
        fileName: data.fileName ?? path,
        extension: data.extension,
        dataBase64: data.dataBase64,
      });
      setOpen(null);
      showToast(`已导入字体：${data.name}`);
    } catch (error) {
      showToast(error instanceof Error ? error.message : String(error));
    } finally {
      setImporting(false);
    }
  };

  return (
    <>
      <button
        ref={triggerRef}
        type="button"
        aria-haspopup="listbox"
        aria-expanded={Boolean(open)}
        onClick={() => (open ? setOpen(null) : openPicker())}
        className="flex h-[28px] w-full max-w-[260px] items-center justify-between gap-2 rounded-[6px] border border-line bg-field px-2 text-left text-[11.5px] text-ink2 transition-colors hover:border-line-strong"
      >
        <span className="truncate">{fontLabel(font, systemName)}</span>
        <ChevronDown size={12} className="shrink-0 text-ink4" />
      </button>

      {open ? (
        <Popover x={open.x} y={open.y} onClose={() => setOpen(null)} width={280} maxHeight={380}>
          <div className="sticky top-0 z-10 bg-layer2 pb-1">
            <div className="relative">
              <Search
                size={11}
                className="pointer-events-none absolute left-2 top-1/2 -translate-y-1/2 text-ink4"
              />
              <Input
                autoFocus
                value={query}
                onChange={(event) => setQuery(event.target.value)}
                placeholder="搜索已安装字体"
                className="h-[26px] pl-6 text-[11px]"
                aria-label="搜索字体"
              />
            </div>
          </div>

          <button
            type="button"
            onClick={() => {
              setFont({ mode: 'system' });
              setOpen(null);
            }}
            className={cx(
              'flex w-full items-center justify-between gap-2 rounded-[5px] px-2 py-1.5 text-left text-[11.5px] transition-colors hover:bg-hover',
              font.mode === 'system' ? 'text-ink' : 'text-ink2',
            )}
          >
            <span className="truncate">跟随系统（{systemName}）</span>
            {font.mode === 'system' ? <Check size={12} className="text-accent" /> : null}
          </button>

          <div className="my-1 h-px bg-line" />
          <div role="listbox" aria-label="字体列表" className="max-h-[190px] overflow-y-auto">
            {loading ? (
              <p className="flex items-center gap-2 px-2 py-2 text-[10.5px] text-ink4">
                <Loader2 size={12} className="animate-spin" />
                正在读取系统字体…
              </p>
            ) : filtered.length === 0 ? (
              <p className="px-2 py-2 text-[10.5px] text-ink4">没有匹配的字体</p>
            ) : (
              filtered.map((name) => {
                const selected = font.mode === 'family' && font.family === name;
                return (
                  <button
                    key={name}
                    type="button"
                    role="option"
                    aria-selected={selected}
                    onClick={() => {
                      setFont({ mode: 'family', family: name });
                      setOpen(null);
                    }}
                    className={cx(
                      'flex h-[26px] w-full items-center justify-between gap-2 rounded-[5px] px-2 text-left text-[11.5px] transition-colors hover:bg-hover',
                      selected ? 'text-ink' : 'text-ink2',
                    )}
                    style={{ fontFamily: `"${name}", var(--font-sans)` }}
                  >
                    <span className="truncate">{name}</span>
                    {selected ? <Check size={12} className="shrink-0 text-accent" /> : null}
                  </button>
                );
              })
            )}
          </div>

          <div className="my-1 h-px bg-line" />
          <button
            type="button"
            disabled={importing}
            onClick={() => void importFont()}
            className="flex w-full items-center gap-2 rounded-[5px] px-2 py-1.5 text-left text-[11.5px] text-ink2 transition-colors hover:bg-hover disabled:opacity-50"
          >
            {importing ? <Loader2 size={12} className="animate-spin" /> : <Upload size={12} />}
            导入字体文件…
          </button>
        </Popover>
      ) : null}
    </>
  );
}
