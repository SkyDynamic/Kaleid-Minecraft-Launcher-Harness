//! 「自动将游戏调为中文」：改 `options.txt` 里的 `lang:` 一行。
//!
//! 三个必须守住的边界：
//! 1. **只动 `lang:` 这一行**，别的设置一个字都不碰（玩家最烦被改键位和画质）。
//! 2. 有存档 + 已经有 `options.txt` 时**跳过**：这时候文件是玩家自己调过的，
//!    再改语言等于替他做决定。
//! 3. 优先写 `config/yosbr/options.txt`（装了 YOSBR 时它会覆盖根目录那份），
//!    否则写根目录的 `options.txt`。为什么要优先 YOSBR：那份文件才是最终生效的，
//!    改根目录会被覆盖回去，用户会以为"自动中文没生效"。

use std::path::{Path, PathBuf};

/// 简体中文在 `options.txt` 里的取值。
///
/// 1.10 及更早用大写的 `zh_CN`（当时语言代码就是 Java 的 Locale 写法），
/// 1.11 起统一成小写 `zh_cn`。写错了游戏会当成未知语言，直接卡在英文。
pub fn lang_value(game_version: &str) -> &'static str {
    if is_legacy_language_code(game_version) {
        "zh_CN"
    } else {
        "zh_cn"
    }
}

/// 版本号里第一段是 1，第二段 ≤ 10 → 老写法。
fn is_legacy_language_code(game_version: &str) -> bool {
    let mut parts = game_version.split(['.', '-', '+']);
    let Some(major) = parts.next().and_then(|p| p.parse::<u32>().ok()) else {
        return false;
    };
    if major != 1 {
        return false;
    }
    matches!(parts.next().and_then(|p| p.parse::<u32>().ok()), Some(minor) if minor <= 10)
}

/// 目标文件：YOSBR 优先，其次根目录。
pub fn target_file(game_dir: &Path) -> PathBuf {
    let yosbr = game_dir.join("config").join("yosbr").join("options.txt");
    if yosbr.is_file() {
        return yosbr;
    }
    game_dir.join("options.txt")
}

/// 把一个 `options.txt` 的内容改成中文，返回新内容。
///
/// 已经有 `lang:` 就原地替换；没有就追加到末尾（并补一个换行）。
///
/// **保留原来的换行风格**：Windows 上游戏写出来的是 CRLF，如果这里统一改成 LF，
/// 虽然游戏读得懂，但整个文件 150 多行都会变，用户用 git/对比工具看就是"全改了"。
pub fn with_chinese_lang(original: &str, game_version: &str) -> String {
    let value = lang_value(game_version);
    let eol = if original.contains("\r\n") { "\r\n" } else { "\n" };
    let mut out = String::with_capacity(original.len() + 16);
    let mut replaced = false;
    for line in original.lines() {
        if line.starts_with("lang:") {
            out.push_str(&format!("lang:{value}"));
            replaced = true;
        } else {
            out.push_str(line);
        }
        out.push_str(eol);
    }
    if !replaced {
        // `lines()` 已经为每一行补了换行，这里直接接在末尾即可
        out.push_str(&format!("lang:{value}{eol}"));
    }
    out
}

/// 执行「自动中文」，返回要显示给用户的说明（空 = 悄悄做完了）。
///
/// 说明里刻意区分"改了 / 原样未动 / 出错了"，因为这三种情况用户的期待完全不同。
///
/// 这个开关本身**就是**用户的授权：开了就照做，哪怕这个版本已经玩过、已经有存档。
/// （早先的版本在有存档时会跳过，结果"我明明开了自动中文，进游戏还是英文"——
/// 把一个用户亲手打开的开关偷偷忽略掉，比改一行 `lang:` 更不礼貌。）
/// 只保证一件事：除 `lang:` 之外的任何内容都不动。
pub fn ensure_chinese(game_dir: &Path, game_version: &str) -> Vec<String> {
    let mut notes = Vec::new();
    if !game_dir.is_dir() {
        notes.push(format!(
            "游戏目录还不存在，自动中文跳过：{}",
            game_dir.display()
        ));
        return notes;
    }
    let target = target_file(game_dir);
    let value = lang_value(game_version);

    let original = std::fs::read_to_string(&target).unwrap_or_default();
    let previous = original
        .lines()
        .find_map(|line| line.strip_prefix("lang:"))
        .map(str::to_string);
    if previous.as_deref() == Some(value) {
        return notes;
    }
    let updated = with_chinese_lang(&original, game_version);
    if let Err(e) = std::fs::write(&target, &updated) {
        notes.push(format!("自动中文失败：{e}"));
        return notes;
    }
    notes.push(match previous {
        Some(before) => format!("已把游戏语言从 {before} 改成简体中文（{value}）"),
        None => format!("已把游戏语言设成简体中文（{value}）"),
    });
    notes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_versions_use_uppercase_code() {
        assert_eq!(lang_value("1.7.10"), "zh_CN");
        assert_eq!(lang_value("1.10.2"), "zh_CN");
        assert_eq!(lang_value("1.11"), "zh_cn");
        assert_eq!(lang_value("1.21.4"), "zh_cn");
        assert_eq!(lang_value("26.3"), "zh_cn");
    }

    #[test]
    fn existing_lang_line_is_replaced_in_place() {
        let original = "version:3465\nlang:en_us\nfov:0.0\n";
        let updated = with_chinese_lang(original, "1.21.4");
        assert!(updated.contains("lang:zh_cn\n"));
        assert!(!updated.contains("lang:en_us"));
        // 其他设置原封不动
        assert!(updated.contains("version:3465"));
        assert!(updated.contains("fov:0.0"));
    }

    #[test]
    fn missing_lang_line_is_appended() {
        let updated = with_chinese_lang("fov:0.0", "1.21.4");
        assert!(updated.starts_with("fov:0.0\n"));
        assert!(updated.ends_with("lang:zh_cn\n"));
    }

    #[test]
    fn crlf_files_keep_their_line_endings() {
        let original = "version:3465\r\nlang:en_us\r\nfov:0.0\r\n";
        let updated = with_chinese_lang(original, "1.21.4");
        assert!(updated.contains("lang:zh_cn\r\n"));
        // 全篇仍是 CRLF：不能因为改一行就把整份文件的换行风格换掉
        assert_eq!(updated.matches("\r\n").count(), 3);
        assert_eq!(updated.matches('\n').count(), 3);
    }

    /// 真机证据：拿这台机器上一份真实 `options.txt`（CRLF，148 行）跑一遍，
    /// 只该动 `lang:` 那一行。文件不存在就跳过。
    ///
    /// `cargo test -p kaleid-core-plugins --lib options::tests::real_options_file_roundtrip -- --ignored --nocapture`
    #[test]
    #[ignore = "读这台机器上的真实游戏目录，手动跑"]
    fn real_options_file_roundtrip() {
        let candidates = [
            r"D:\MC\.minecraft\versions\1.21.4-Fabric 0.16.10\options.txt",
            r"C:\Users\SkyDy\AppData\Roaming\.minecraft\options.txt",
        ];
        let Some(path) = candidates.iter().find(|p| Path::new(p).is_file()) else {
            println!("这台机器上没有真实 options.txt，跳过");
            return;
        };
        let original = std::fs::read_to_string(path).unwrap();

        let dir = tempfile::tempdir().unwrap();
        let game_dir = dir.path();
        let switched = original.replace("lang:zh_cn", "lang:en_us");
        std::fs::write(game_dir.join("options.txt"), &switched).unwrap();

        let notes = ensure_chinese(game_dir, "1.21.4");
        println!("notes: {notes:?}");
        let updated = std::fs::read_to_string(game_dir.join("options.txt")).unwrap();

        assert!(updated.contains("lang:zh_cn"));
        assert!(!updated.contains("lang:en_us"));
        // 换行风格与行数都保持不变
        assert_eq!(
            updated.matches('\n').count(),
            switched.matches('\n').count(),
            "行数不该变"
        );
        assert_eq!(
            updated.matches("\r\n").count(),
            switched.matches("\r\n").count(),
            "CRLF 数量不该变"
        );
        // 除 lang 之外逐行相同
        let before: Vec<&str> = switched.lines().filter(|l| !l.starts_with("lang:")).collect();
        let after: Vec<&str> = updated.lines().filter(|l| !l.starts_with("lang:")).collect();
        assert_eq!(before, after, "除 lang 之外的行必须一字不动");
    }

    #[test]
    fn yosbr_file_wins_when_present() {
        let dir = tempfile::tempdir().unwrap();
        let game_dir = dir.path();
        std::fs::create_dir_all(game_dir.join("config/yosbr")).unwrap();
        std::fs::write(game_dir.join("config/yosbr/options.txt"), "lang:en_us\n").unwrap();
        assert_eq!(
            target_file(game_dir),
            game_dir.join("config").join("yosbr").join("options.txt")
        );
        let notes = ensure_chinese(game_dir, "1.21.4");
        assert!(notes.iter().any(|n| n.contains("简体中文")));
        let text = std::fs::read_to_string(target_file(game_dir)).unwrap();
        assert!(text.contains("lang:zh_cn"));
    }

    #[test]
    fn played_saves_still_follow_the_switch() {
        // 开关是用户亲手打开的：已经有存档也照做，但只动 lang 那一行
        let dir = tempfile::tempdir().unwrap();
        let game_dir = dir.path();
        std::fs::create_dir_all(game_dir.join("saves/我的世界")).unwrap();
        std::fs::write(game_dir.join("options.txt"), "fov:0.0\nlang:en_us\n").unwrap();
        let notes = ensure_chinese(game_dir, "1.21.4");
        assert!(notes.iter().any(|n| n.contains("en_us")), "说明里要写清原来是什么");
        let text = std::fs::read_to_string(game_dir.join("options.txt")).unwrap();
        assert_eq!(text, "fov:0.0\nlang:zh_cn\n");
    }

    #[test]
    fn already_chinese_is_a_no_op() {
        let dir = tempfile::tempdir().unwrap();
        let game_dir = dir.path();
        std::fs::write(game_dir.join("options.txt"), "lang:zh_cn\n").unwrap();
        let notes = ensure_chinese(game_dir, "1.21.4");
        assert!(notes.is_empty());
    }

    #[test]
    fn missing_game_dir_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let notes = ensure_chinese(&dir.path().join("nope"), "1.21.4");
        assert!(notes.iter().any(|n| n.contains("不存在")));
    }
}
