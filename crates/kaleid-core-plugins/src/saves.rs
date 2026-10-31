//! 存档（`saves/`）的列表、删除，以及单个存档的 datapacks 查/增/删。
//!
//! 这里刻意**没有**"启用/禁用 datapack"的写接口：启用状态存在 `level.dat`
//! （gzip 压缩的 NBT）的 `DataPacks.Enabled` / `DataPacks.Disabled` 里，改它要
//! 有 NBT 写入器并把整份存档数据重新编码落盘 —— 一次字节错位就是玩家存档报废。
//! 本模块只借 [`crate::nbt::read_level_dat`] 把状态读出来给界面显示，真正的
//! 启用/禁用留给游戏自己写。
//!
//! 另一个取舍是**只碰 `saves/` 下的东西**：删除是"删目录"，不会去动
//! `level.dat`、不会动全局设置，因此失败面很小；反过来 `folder` / `file_name`
//! 都是界面直传的字符串，必须当成敌意输入逐段校验（见 [`validate_component`]），
//! 否则 `..` 就能让"删一个存档"变成删别的东西。

use std::path::{Path, PathBuf};

use serde_json::Value;
use walkdir::WalkDir;

use kaleid_api::{ApiError, ApiResult, DataPack, SaveDeleteOutcome};

/// 统计一个目录体积时最多看这么多文件。
///
/// 上限存在的理由：`region/` 里放的是几 GiB 的 mca，几百个存档一轮列表就是
/// 数万次 `stat`，界面会卡在"加载中"。宁可少报一点字节数（只用于展示和删除
/// 后的"释放空间"提示），也不要为了精确而让列表转圈。
const MAX_WALK_FILES: usize = 20_000;

/// `level.dat` 里 `icon` 是 base64 后的 PNG 文本，超过这个长度就不再拼成
/// data URL。512 KiB 的 base64 约等于 384 KiB 的图，正常图标远小于此；
/// 再大的"图标"多半是别人塞进去的杂物，塞进界面只会拖慢渲染。
const MAX_ICON_BASE64: usize = 512 * 1024;

/// 读 `pack.mcmeta` 的上限：它就是一个几百字节的 JSON。
const MAX_MCMETA_BYTES: usize = 64 * 1024;

/// 存档列表里的一行（比 DataPack 多，供界面直接显示）。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveSummary {
    /// 目录名，也是对外的"存档 id"。
    pub folder: String,
    /// `level.dat` 的 `LevelName`；读不到（老存档、文件损坏）就回落成目录名。
    pub display_name: String,
    /// `level.dat` 的 `LastPlayed`（毫秒时间戳）。没有就不排序依据。
    pub last_played: Option<u64>,
    pub size_bytes: u64,
    /// `level.dat` 的 `GameType`：0 生存 / 1 创造 / 2 冒险 / 3 旁观。
    pub game_mode: Option<i32>,
    /// `level.dat` 的 `Version.Name`。
    pub version_name: Option<String>,
    /// `level.dat` 的 `icon` 拼成的 base64 PNG data URL。
    pub icon_data_url: Option<String>,
    pub datapack_count: usize,
}

/// 存档管理动作的实现。
pub struct SaveManager {
    /// 数据根目录（与其它服务保持一致的构造签名）。删档当前是直接删目录，
    /// 这个字段留给以后可能加的"回收站"或删除日志，现在只做记录不参与逻辑。
    #[allow(dead_code)]
    data_root: PathBuf,
}

impl SaveManager {
    pub fn new(data_root: &Path) -> Self {
        SaveManager {
            data_root: data_root.to_path_buf(),
        }
    }

    /// 列出一个游戏目录（`.minecraft` 根或版本隔离目录）里的存档。
    ///
    /// `saves/` 不存在只是"还没玩过"，返回空列表而不是错误：首次启动的目录树
    /// 里根本没有这一层，把它报成错误会让界面一进来就弹红字。
    pub fn list(&self, game_dir: &Path) -> ApiResult<Vec<SaveSummary>> {
        let saves = game_dir.join("saves");
        if !saves.is_dir() {
            return Ok(Vec::new());
        }
        let entries = std::fs::read_dir(&saves)
            .map_err(|e| ApiError::io(format!("读不了 {} 目录：{e}", saves.display())))?;

        let mut out = Vec::new();
        for entry in entries.flatten() {
            let folder = entry.file_name().to_string_lossy().into_owned();
            // 以点开头的是系统垃圾（`.DS_Store`、编辑器临时目录），不是存档。
            if folder.starts_with('.') {
                continue;
            }
            // 只要目录：`saves/` 下可能被人放进说明文本、备份 zip 之类。
            match entry.metadata() {
                Ok(meta) if meta.is_dir() => {}
                _ => continue,
            }
            out.push(self.summarize(&entry.path(), folder));
        }
        // 最近玩过的在最前：界面默认就该是"我上次玩的那个"。没时间的排最后。
        out.sort_by(|a, b| {
            b.last_played
                .cmp(&a.last_played)
                .then_with(|| a.folder.cmp(&b.folder))
        });
        Ok(out)
    }

    /// 删除一个存档目录。
    ///
    /// 先量体积再删（删完就量不了了）。量体积用的是带文件数上限的 [`dir_size`]，
    /// 所以超大存档报出来的"释放空间"可能偏小，但不会拖住界面。
    pub fn delete(&self, game_dir: &Path, folder: &str) -> ApiResult<SaveDeleteOutcome> {
        let dir = self.save_dir(game_dir, folder)?;
        let freed = dir_size(&dir);
        std::fs::remove_dir_all(&dir).map_err(|e| {
            ApiError::io(format!(
                "删除 {} 失败：{e}（如果游戏正在运行，请先退出游戏）",
                dir.display()
            ))
        })?;
        Ok(SaveDeleteOutcome {
            folder: folder.to_string(),
            freed_bytes: freed,
        })
    }

    /// 该存档的 datapacks（`<save>/datapacks/` 下的 zip 与目录）。
    ///
    /// `enabled` 来自 `level.dat`，只是**读**出来的展示值；`None` 表示
    /// `level.dat` 里没提到它（比如从没在游戏里启用过，或文件是后塞进去的）。
    pub fn datapacks(&self, game_dir: &Path, folder: &str) -> ApiResult<Vec<DataPack>> {
        let save = self.save_dir(game_dir, folder)?;
        let packs = level_datapacks(&save);
        let mut out = Vec::new();
        for (file_name, is_dir) in datapack_entries(&save) {
            let path = save.join("datapacks").join(&file_name);
            let size_bytes = if is_dir {
                dir_size(&path)
            } else {
                std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0)
            };
            let (pack_format, description) = pack_meta(&path, is_dir);
            out.push(DataPack {
                enabled: enabled_state(packs.as_ref(), &file_name),
                kind: if is_dir { "dir" } else { "zip" }.to_string(),
                file_name,
                size_bytes,
                pack_format,
                description,
            });
        }
        Ok(out)
    }

    /// 把外部 zip 复制进 `<save>/datapacks/`；返回落盘后的文件名。
    ///
    /// 重名不改写已有的包（那等于悄悄替换玩家的数据），而是在文件名后加
    /// `-1`、`-2`……，把"两个版本并存"的事实摆到界面上。
    pub fn import_datapack(
        &self,
        game_dir: &Path,
        folder: &str,
        source: &Path,
    ) -> ApiResult<String> {
        let save = self.save_dir(game_dir, folder)?;
        if !source.is_file() {
            return Err(ApiError::not_found(format!(
                "找不到要导入的文件：{}",
                source.display()
            )));
        }
        // 只收 zip：目录形式的 datapack 要整棵树复制，导入一个"文件"的语义
        // 不该悄悄变成"导入一堆文件"。
        if !has_zip_extension(source) {
            return Err(ApiError::invalid(format!(
                "{} 不是 .zip 文件，datapack 只能是 zip",
                source.display()
            )));
        }
        let file_name = source
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .ok_or_else(|| ApiError::invalid("源路径没有文件名"))?;

        let dest = save.join("datapacks");
        std::fs::create_dir_all(&dest)
            .map_err(|e| ApiError::io(format!("建不了 {} 目录：{e}", dest.display())))?;
        let target_name = unique_datapack_name(&dest, &file_name)?;
        let target = dest.join(&target_name);
        std::fs::copy(source, &target)
            .map_err(|e| ApiError::io(format!("复制到 {} 失败：{e}", target.display())))?;
        Ok(target_name)
    }

    /// 删除一个 datapack（zip 或目录）。
    pub fn delete_datapack(&self, game_dir: &Path, folder: &str, file_name: &str) -> ApiResult<()> {
        let save = self.save_dir(game_dir, folder)?;
        // 文件名同样是界面直传的输入：不校验的话 `../level.dat` 就能删到存档本体。
        validate_component(file_name, "datapack 文件名")?;
        let path = save.join("datapacks").join(file_name);
        let meta = std::fs::metadata(&path)
            .map_err(|_| ApiError::not_found(format!("datapack {file_name} 不存在")))?;
        let removed = if meta.is_dir() {
            std::fs::remove_dir_all(&path)
        } else {
            std::fs::remove_file(&path)
        };
        removed.map_err(|e| ApiError::io(format!("删除 {} 失败：{e}", path.display())))
    }

    /// 校验并返回一个确实存在的存档目录。
    fn save_dir(&self, game_dir: &Path, folder: &str) -> ApiResult<PathBuf> {
        validate_component(folder, "存档目录名")?;
        let dir = game_dir.join("saves").join(folder);
        if !dir.is_dir() {
            return Err(ApiError::not_found(format!("存档 {folder} 不存在")));
        }
        Ok(dir)
    }

    /// 读一个存档目录的展示信息。`level.dat` 读不到不算错：剩下的大小、目录名
    /// 依然有用，界面可以先列出来再提示"这个存档可能损坏"。
    fn summarize(&self, dir: &Path, folder: String) -> SaveSummary {
        let level = crate::nbt::read_level_dat(&dir.join("level.dat")).ok();
        let data = level.as_ref().and_then(|v| v.get("Data"));

        let display_name = data
            .and_then(|d| d.get("LevelName"))
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| folder.clone());

        let last_played = data
            .and_then(|d| d.get("LastPlayed"))
            .and_then(|v| v.as_u64());

        let game_mode = data
            .and_then(|d| d.get("GameType"))
            .and_then(|v| v.as_i64())
            .map(|v| v as i32);

        let version_name = data
            .and_then(|d| d.get("Version"))
            .and_then(|v| v.get("Name"))
            .and_then(|v| v.as_str())
            .map(str::to_string);

        let icon_data_url = data
            .and_then(|d| d.get("icon"))
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|b64| !b64.is_empty() && b64.len() < MAX_ICON_BASE64)
            .map(|b64| format!("data:image/png;base64,{b64}"));

        SaveSummary {
            folder,
            display_name,
            last_played,
            size_bytes: dir_size(dir),
            game_mode,
            version_name,
            icon_data_url,
            datapack_count: datapack_entries(dir).len(),
        }
    }
}

/// 校验一个用户可控的路径分段（存档目录名 / datapack 文件名）。
///
/// 只接受单段名字：拒绝空、`.`、`..`、含分隔符（`/` `\` `:`）以及以 `.` 开头
/// 的隐藏名。`folder` 与 `file_name` 都会直接拼到路径上，放进来一个 `..` 就
/// 等价于"把删除/写入重定向到别处"，所以这里是唯一的守门点。
fn validate_component(value: &str, what: &str) -> ApiResult<()> {
    if value.trim().is_empty() {
        return Err(ApiError::invalid(format!("{what}不能为空")));
    }
    if value != value.trim() {
        return Err(ApiError::invalid(format!(
            "{what}首尾不能有空白：{value:?}"
        )));
    }
    if value == "." || value == ".." {
        return Err(ApiError::invalid(format!("{what}不能是 {value}")));
    }
    if value.starts_with('.') {
        return Err(ApiError::invalid(format!(
            "{what}不能以 . 开头（系统隐藏项不当作存档内容）：{value}"
        )));
    }
    if value.contains(['/', '\\', ':']) || value.contains('\0') {
        return Err(ApiError::invalid(format!(
            "{what}只能是单段名字，不能含路径分隔符：{value}"
        )));
    }
    Ok(())
}

/// 递归累加目录里的文件体积，最多看 [`MAX_WALK_FILES`] 个文件。
///
/// 读不到权限/坏链接的条目直接跳过（`flatten`）：量不出来就当 0 字节，
/// 不该让一次列表因为一个坏文件整份失败。
fn dir_size(dir: &Path) -> u64 {
    let mut total = 0u64;
    let mut seen = 0usize;
    for entry in WalkDir::new(dir).into_iter().flatten() {
        if !entry.file_type().is_file() {
            continue;
        }
        seen += 1;
        if let Ok(meta) = entry.metadata() {
            total = total.saturating_add(meta.len());
        }
        if seen >= MAX_WALK_FILES {
            break;
        }
    }
    total
}

/// `<save>/datapacks/` 下的候选包：`(名字, 是否目录)`，按名字排序。
///
/// 只认目录与 `.zip`：游戏本身也是这么扫的，别的文件放进去不会生效，
/// 列出来只会让界面多一行点不动的条目。
fn datapack_entries(save: &Path) -> Vec<(String, bool)> {
    let dir = save.join("datapacks");
    let Ok(read) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in read.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        if meta.is_dir() {
            out.push((name, true));
        } else if meta.is_file() && has_zip_extension(&entry.path()) {
            out.push((name, false));
        }
    }
    out.sort();
    out
}

/// 扩展名判断不区分大小写：`MyPack.ZIP` 在 Windows 上就是 `MyPack.zip`。
fn has_zip_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.eq_ignore_ascii_case("zip"))
        .unwrap_or(false)
}

/// 取 `level.dat` 里的 `Data.DataPacks`。读不到就是 `None`，全体的启用状态
/// 都退化成"未知"，而不是猜成"未启用"。
fn level_datapacks(save: &Path) -> Option<Value> {
    let value = crate::nbt::read_level_dat(&save.join("level.dat")).ok()?;
    value.get("Data")?.get("DataPacks").cloned()
}

/// 一个包在 `level.dat` 里的启用状态。
///
/// 先看 `Enabled` 再看 `Disabled`：同一份文件理论上不该两边都有，真出现了
/// 就按"启用"显示（游戏的实际行为是后者覆盖前者，这里只展示不替游戏下结论）。
fn enabled_state(packs: Option<&Value>, file_name: &str) -> Option<bool> {
    let packs = packs?;
    let target = normalize_pack_name(file_name);
    if list_has(packs.get("Enabled"), &target) {
        return Some(true);
    }
    if list_has(packs.get("Disabled"), &target) {
        return Some(false);
    }
    None
}

fn list_has(list: Option<&Value>, target: &str) -> bool {
    list.and_then(|v| v.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|v| v.as_str())
                .any(|name| normalize_pack_name(name) == target)
        })
        .unwrap_or(false)
}

/// 把 `level.dat` 里的包名和磁盘上的名字归一到同一形态再比较。
///
/// 游戏对 zip 的记法是 `file/xxx.zip`（目录包则只写目录名），而且 Windows 文件
/// 系统本身不区分大小写，所以这里统一去前缀、去 `.zip`、转小写。
/// 内置包（`vanilla`、`fabric`）在磁盘上没有对应条目，天然不会被匹配到。
fn normalize_pack_name(name: &str) -> String {
    let name = name.trim();
    let name = name.strip_prefix("file/").unwrap_or(name);
    // `.zip` 是纯 ASCII，据此切字节不会切在多字节字符中间。
    let lower = name.to_ascii_lowercase();
    let stem = if lower.ends_with(".zip") {
        &name[..name.len() - 4]
    } else {
        name
    };
    stem.to_ascii_lowercase()
}

/// 从 `pack.mcmeta` 取 `pack_format` 与说明。
///
/// 读不到、解析不了都只返回 `(None, None)`：缺元数据不该让整份列表失败 ——
/// 一个手改过的包在游戏里照样能加载。
fn pack_meta(path: &Path, is_dir: bool) -> (Option<u32>, Option<String>) {
    let raw = if is_dir {
        let file = path.join("pack.mcmeta");
        match std::fs::metadata(&file) {
            Ok(meta) if meta.len() <= MAX_MCMETA_BYTES as u64 => std::fs::read(&file).ok(),
            _ => None,
        }
    } else {
        // zip 里的条目走 jar 的中央目录读取；没有 mcmeta 是正常情况。
        crate::jar::read_entry_bytes(path, "pack.mcmeta", MAX_MCMETA_BYTES)
    };
    let Some(raw) = raw else {
        return (None, None);
    };
    let Ok(value) = serde_json::from_slice::<Value>(&raw) else {
        return (None, None);
    };
    let pack = value.get("pack");
    let pack_format = pack
        .and_then(|p| p.get("pack_format"))
        .and_then(|v| v.as_u64())
        .and_then(|v| u32::try_from(v).ok());
    (
        pack_format,
        pack.and_then(|p| p.get("description"))
            .and_then(description_text),
    )
}

/// 说明可能是字符串，也可能是 `{"text": ...}` / `{"translate": ...}` 这类
/// 原版聊天组件。只取 `text`：`translate` 需要语言表才能变成人话，
/// 拿键名冒充说明不如不显示。
fn description_text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => {
            let text = text.trim();
            (!text.is_empty()).then(|| text.to_string())
        }
        Value::Object(map) => map
            .get("text")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(str::to_string),
        _ => None,
    }
}

/// 在目标目录里挑一个不撞名的文件名：`name.zip` → `name-1.zip` → `name-2.zip`…
///
/// 撞名判断用小写集合而不是逐次 `exists()`：Windows 上 `Exists` 已经是不区分
/// 大小写的，但先把目录读一遍只花一次系统调用，逐个试是 N 次。
fn unique_datapack_name(dir: &Path, file_name: &str) -> ApiResult<String> {
    let mut taken: std::collections::HashSet<String> = std::collections::HashSet::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            taken.insert(entry.file_name().to_string_lossy().to_ascii_lowercase());
        }
    }
    if !taken.contains(&file_name.to_ascii_lowercase()) {
        return Ok(file_name.to_string());
    }
    let (stem, ext) = split_name(file_name);
    for n in 1..=9999u32 {
        let candidate = format!("{stem}-{n}{ext}");
        if !taken.contains(&candidate.to_ascii_lowercase()) {
            return Ok(candidate);
        }
    }
    Err(ApiError::conflict(format!(
        "{} 里同名文件太多了，换个名字再导入",
        dir.display()
    )))
}

/// 拆成 `(主干, 扩展名含点)`；`split_name("a.zip") == ("a", ".zip")`。
fn split_name(file_name: &str) -> (&str, &str) {
    match file_name.rfind('.') {
        Some(at) if at > 0 => (&file_name[..at], &file_name[at..]),
        _ => (file_name, ""),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::write::GzEncoder;
    use flate2::Compression;
    use std::io::Write;

    // ---- NBT / gzip fixture ------------------------------------------------
    //
    // `nbt.rs` 只有读没有写，所以测试自己拼最小 NBT：根 Compound + `Data`
    // Compound + 几个标量/字符串列表，再用 GzEncoder 压一遍。只写测试需要
    // 的那几种标签，不需要通用写入器。

    const TAG_END: u8 = 0;
    const TAG_INT: u8 = 3;
    const TAG_LONG: u8 = 4;
    const TAG_STRING: u8 = 8;
    const TAG_LIST: u8 = 9;
    const TAG_COMPOUND: u8 = 10;

    fn head(out: &mut Vec<u8>, tag: u8, name: &str) {
        out.push(tag);
        out.extend_from_slice(&(name.len() as u16).to_be_bytes());
        out.extend_from_slice(name.as_bytes());
    }

    fn nbt_string(out: &mut Vec<u8>, name: &str, value: &str) {
        head(out, TAG_STRING, name);
        out.extend_from_slice(&(value.len() as u16).to_be_bytes());
        out.extend_from_slice(value.as_bytes());
    }

    fn nbt_int(out: &mut Vec<u8>, name: &str, value: i32) {
        head(out, TAG_INT, name);
        out.extend_from_slice(&value.to_be_bytes());
    }

    fn nbt_long(out: &mut Vec<u8>, name: &str, value: i64) {
        head(out, TAG_LONG, name);
        out.extend_from_slice(&value.to_be_bytes());
    }

    /// 字符串列表（TAG_List 的元素类型为 TAG_String）。
    fn nbt_str_list(out: &mut Vec<u8>, name: &str, items: &[&str]) {
        head(out, TAG_LIST, name);
        out.push(TAG_STRING);
        out.extend_from_slice(&(items.len() as i32).to_be_bytes());
        for item in items {
            out.extend_from_slice(&(item.len() as u16).to_be_bytes());
            out.extend_from_slice(item.as_bytes());
        }
    }

    /// 测试用 `level.dat` 的内容。
    struct Level {
        level_name: &'static str,
        game_type: i32,
        last_played: i64,
        version: &'static str,
        icon: Option<&'static str>,
        enabled: &'static [&'static str],
        disabled: &'static [&'static str],
    }

    impl Default for Level {
        fn default() -> Self {
            Level {
                level_name: "World",
                game_type: 1,
                last_played: 1_700_000_000_000,
                version: "1.21.4",
                icon: None,
                enabled: &[],
                disabled: &[],
            }
        }
    }

    /// 拼出未压缩的 NBT 再 gzip 成 `level.dat` 的字节。
    fn level_bytes(level: &Level) -> Vec<u8> {
        let mut raw = Vec::new();
        raw.push(TAG_COMPOUND);
        raw.extend_from_slice(&0u16.to_be_bytes()); // 根名字为空
        head(&mut raw, TAG_COMPOUND, "Data");
        nbt_string(&mut raw, "LevelName", level.level_name);
        nbt_int(&mut raw, "GameType", level.game_type);
        nbt_long(&mut raw, "LastPlayed", level.last_played);
        if let Some(icon) = level.icon {
            nbt_string(&mut raw, "icon", icon);
        }
        head(&mut raw, TAG_COMPOUND, "Version");
        nbt_string(&mut raw, "Name", level.version);
        raw.push(TAG_END); // Version
        head(&mut raw, TAG_COMPOUND, "DataPacks");
        nbt_str_list(&mut raw, "Enabled", level.enabled);
        nbt_str_list(&mut raw, "Disabled", level.disabled);
        raw.push(TAG_END); // DataPacks
        raw.push(TAG_END); // Data
        raw.push(TAG_END); // 根

        let mut enc = GzEncoder::new(Vec::new(), Compression::fast());
        enc.write_all(&raw).expect("写内存不会失败");
        enc.finish().expect("写内存不会失败")
    }

    /// 在 `saves/` 下造一个存档目录，返回它的路径。
    fn make_save(saves: &Path, folder: &str, level: &Level) -> PathBuf {
        let dir = saves.join(folder);
        std::fs::create_dir_all(dir.join("region")).unwrap();
        std::fs::write(dir.join("level.dat"), level_bytes(level)).unwrap();
        // 放一个区域文件，保证 size_bytes > 0 且和 level.dat 一起被统计。
        std::fs::write(dir.join("region").join("r.0.0.mca"), b"0123456789").unwrap();
        dir
    }

    // ---- zip fixture -------------------------------------------------------
    //
    // 没有 zip 写库，datapack 的 pack.mcmeta 只能自己拼（stored 方式即可，
    // jar 读取器对 method 0 / 8 都支持）。

    fn crc32(data: &[u8]) -> u32 {
        let mut crc = 0xffff_ffffu32;
        for byte in data {
            crc ^= u32::from(*byte);
            for _ in 0..8 {
                let mask = (crc & 1).wrapping_neg();
                crc = (crc >> 1) ^ (0xedb8_8320 & mask);
            }
        }
        !crc
    }

    fn zip_stored(files: &[(&str, &[u8])]) -> Vec<u8> {
        fn u16le(out: &mut Vec<u8>, value: u16) {
            out.extend_from_slice(&value.to_le_bytes());
        }
        fn u32le(out: &mut Vec<u8>, value: u32) {
            out.extend_from_slice(&value.to_le_bytes());
        }

        let mut out: Vec<u8> = Vec::new();
        let mut central: Vec<u8> = Vec::new();
        for (name, data) in files {
            let crc = crc32(data);
            let offset = out.len() as u32;

            u32le(&mut out, 0x0403_4b50); // 本地文件头
            u16le(&mut out, 20);
            u16le(&mut out, 0); // 标志位
            u16le(&mut out, 0); // method 0 = stored
            u16le(&mut out, 0); // 时间
            u16le(&mut out, 0); // 日期
            u32le(&mut out, crc);
            u32le(&mut out, data.len() as u32);
            u32le(&mut out, data.len() as u32);
            u16le(&mut out, name.len() as u16);
            u16le(&mut out, 0); // extra
            out.extend_from_slice(name.as_bytes());
            out.extend_from_slice(data);

            u32le(&mut central, 0x0201_4b50); // 中央目录项
            u16le(&mut central, 20);
            u16le(&mut central, 20);
            u16le(&mut central, 0);
            u16le(&mut central, 0); // method
            u16le(&mut central, 0);
            u16le(&mut central, 0);
            u32le(&mut central, crc);
            u32le(&mut central, data.len() as u32);
            u32le(&mut central, data.len() as u32);
            u16le(&mut central, name.len() as u16);
            u16le(&mut central, 0); // extra
            u16le(&mut central, 0); // 注释
            u16le(&mut central, 0); // 磁盘号
            u16le(&mut central, 0); // 内部属性
            u32le(&mut central, 0); // 外部属性
            u32le(&mut central, offset);
            central.extend_from_slice(name.as_bytes());
        }

        let cd_offset = out.len() as u32;
        let cd_size = central.len() as u32;
        out.extend_from_slice(&central);

        u32le(&mut out, 0x0605_4b50); // EOCD
        u16le(&mut out, 0);
        u16le(&mut out, 0);
        u16le(&mut out, files.len() as u16);
        u16le(&mut out, files.len() as u16);
        u32le(&mut out, cd_size);
        u32le(&mut out, cd_offset);
        u16le(&mut out, 0);
        out
    }

    fn write_zip(path: &Path, files: &[(&str, &[u8])]) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, zip_stored(files)).unwrap();
    }

    fn manager() -> (tempfile::TempDir, SaveManager) {
        let tmp = tempfile::tempdir().unwrap();
        let svc = SaveManager::new(&tmp.path().join("data"));
        (tmp, svc)
    }

    // ---- 列表 --------------------------------------------------------------

    #[test]
    fn lists_two_saves_with_level_dat_fields() {
        let (tmp, svc) = manager();
        let saves = tmp.path().join("saves");
        make_save(
            &saves,
            "World A",
            &Level {
                level_name: "我的世界",
                game_type: 0,
                last_played: 1_600_000_000_000,
                version: "1.20.1",
                icon: Some("iVBORw0KGgo="),
                ..Level::default()
            },
        );
        make_save(
            &saves,
            "World B",
            &Level {
                level_name: "Second",
                game_type: 1,
                last_played: 1_700_000_000_000,
                version: "1.21.4",
                ..Level::default()
            },
        );

        let list = svc.list(tmp.path()).unwrap();
        assert_eq!(list.len(), 2);
        // 最近玩过的排前面。
        assert_eq!(list[0].folder, "World B");
        assert_eq!(list[0].display_name, "Second");
        assert_eq!(list[0].version_name.as_deref(), Some("1.21.4"));
        assert_eq!(list[0].last_played, Some(1_700_000_000_000));
        assert_eq!(list[0].game_mode, Some(1));
        assert!(list[0].size_bytes > 0);
        assert_eq!(list[0].icon_data_url, None);
        assert_eq!(list[0].datapack_count, 0);

        assert_eq!(list[1].folder, "World A");
        assert_eq!(list[1].display_name, "我的世界");
        assert_eq!(list[1].version_name.as_deref(), Some("1.20.1"));
        assert_eq!(list[1].last_played, Some(1_600_000_000_000));
        assert_eq!(list[1].game_mode, Some(0));
        assert_eq!(
            list[1].icon_data_url.as_deref(),
            Some("data:image/png;base64,iVBORw0KGgo=")
        );
    }

    #[test]
    fn missing_saves_dir_is_empty_not_an_error() {
        let (tmp, svc) = manager();
        // 连 .minecraft 都还没建。
        assert!(svc.list(tmp.path()).unwrap().is_empty());
        std::fs::create_dir_all(tmp.path().join("saves")).unwrap();
        assert!(svc.list(tmp.path()).unwrap().is_empty());
    }

    #[test]
    fn skips_hidden_dirs_and_plain_files() {
        let (tmp, svc) = manager();
        let saves = tmp.path().join("saves");
        make_save(&saves, "Keep", &Level::default());
        std::fs::create_dir_all(saves.join(".DS_Store")).unwrap();
        std::fs::create_dir_all(saves.join(".hidden-backup")).unwrap();
        std::fs::write(saves.join("notes.txt"), b"hello").unwrap();

        let list = svc.list(tmp.path()).unwrap();
        assert_eq!(list.len(), 1, "只有 Keep 是存档：{list:?}");
        assert_eq!(list[0].folder, "Keep");
    }

    #[test]
    fn unreadable_level_dat_falls_back_to_folder_name() {
        let (tmp, svc) = manager();
        let saves = tmp.path().join("saves");
        make_save(&saves, "Broken", &Level::default());
        std::fs::write(saves.join("Broken").join("level.dat"), b"not nbt").unwrap();

        let list = svc.list(tmp.path()).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].display_name, "Broken");
        assert_eq!(list[0].version_name, None);
        assert_eq!(list[0].last_played, None);
        assert!(list[0].size_bytes > 0);
    }

    // ---- 删除存档 ----------------------------------------------------------

    #[test]
    fn delete_removes_the_folder_and_reports_freed_bytes() {
        let (tmp, svc) = manager();
        let saves = tmp.path().join("saves");
        let dir = make_save(&saves, "Doomed", &Level::default());
        let expected = svc.list(tmp.path()).unwrap()[0].size_bytes;
        assert!(expected > 0);

        let outcome = svc.delete(tmp.path(), "Doomed").unwrap();
        assert_eq!(outcome.folder, "Doomed");
        assert_eq!(outcome.freed_bytes, expected);
        assert!(!dir.exists());
        assert!(svc.list(tmp.path()).unwrap().is_empty());
    }

    #[test]
    fn delete_unknown_save_is_not_found() {
        let (tmp, svc) = manager();
        std::fs::create_dir_all(tmp.path().join("saves")).unwrap();
        let err = svc.delete(tmp.path(), "Nope").unwrap_err();
        assert!(matches!(err, ApiError::NotFound(_)), "{err:?}");
    }

    #[test]
    fn hostile_folder_names_are_rejected() {
        let (tmp, svc) = manager();
        let saves = tmp.path().join("saves");
        make_save(&saves, "Real", &Level::default());
        // 存档本体挪走，确保任何一次成功的删除都会留下明显痕迹。
        let victim = saves.join("Real");

        for bad in ["", ".", "..", "../Real", "a/b", "a\\b", "C:Real", ".hidden"] {
            let err = svc.delete(tmp.path(), bad).unwrap_err();
            assert!(
                matches!(err, ApiError::Invalid(_)),
                "delete({bad:?}) 应当被拒，实际是 {err:?}"
            );
            let err = svc.datapacks(tmp.path(), bad).unwrap_err();
            assert!(
                matches!(err, ApiError::Invalid(_)),
                "datapacks({bad:?}) 应当被拒，实际是 {err:?}"
            );
            let err = svc
                .import_datapack(tmp.path(), bad, &tmp.path().join("x.zip"))
                .unwrap_err();
            assert!(
                matches!(err, ApiError::Invalid(_)),
                "import({bad:?}) 应当被拒，实际是 {err:?}"
            );
            let err = svc.delete_datapack(tmp.path(), bad, "x.zip").unwrap_err();
            assert!(
                matches!(err, ApiError::Invalid(_)),
                "delete_datapack({bad:?}) 应当被拒，实际是 {err:?}"
            );
        }
        assert!(victim.is_dir(), "被拒绝的调用不能动到真实存档");
    }

    #[test]
    fn hostile_datapack_names_are_rejected() {
        let (tmp, svc) = manager();
        let saves = tmp.path().join("saves");
        let dir = make_save(&saves, "Real", &Level::default());
        for bad in ["", ".", "..", "../level.dat", "a/b.zip", ".hidden.zip"] {
            let err = svc.delete_datapack(tmp.path(), "Real", bad).unwrap_err();
            assert!(
                matches!(err, ApiError::Invalid(_)),
                "delete_datapack({bad:?}) 应当被拒，实际是 {err:?}"
            );
        }
        assert!(dir.join("level.dat").is_file());
    }

    // ---- datapacks ---------------------------------------------------------

    #[test]
    fn lists_zip_and_dir_datapacks_with_meta_and_enabled_state() {
        let (tmp, svc) = manager();
        let saves = tmp.path().join("saves");
        let dir = make_save(
            &saves,
            "Packed",
            &Level {
                enabled: &["vanilla", "file/MyPack.zip"],
                disabled: &["dirpack"],
                ..Level::default()
            },
        );
        let packs = dir.join("datapacks");
        write_zip(
            &packs.join("MyPack.zip"),
            &[(
                "pack.mcmeta",
                // 非 ASCII 的 JSON 不能用 byte string，转成字节切片。
                r#"{"pack":{"pack_format":48,"description":{"text":"我的数据包"}}}"#.as_bytes(),
            )],
        );
        std::fs::create_dir_all(packs.join("dirpack")).unwrap();
        std::fs::write(
            packs.join("dirpack").join("pack.mcmeta"),
            r#"{"pack":{"pack_format":15,"description":"目录形态"}}"#.as_bytes(),
        )
        .unwrap();
        // 不是 zip 的杂物不该出现在列表里。
        std::fs::write(packs.join("readme.txt"), b"ignore me").unwrap();

        let list = svc.datapacks(tmp.path(), "Packed").unwrap();
        assert_eq!(list.len(), 2, "{list:?}");

        let zip = list.iter().find(|p| p.file_name == "MyPack.zip").unwrap();
        assert_eq!(zip.kind, "zip");
        assert_eq!(zip.enabled, Some(true));
        assert_eq!(zip.pack_format, Some(48));
        assert_eq!(zip.description.as_deref(), Some("我的数据包"));
        assert!(zip.size_bytes > 0);

        let folder = list.iter().find(|p| p.file_name == "dirpack").unwrap();
        assert_eq!(folder.kind, "dir");
        assert_eq!(folder.enabled, Some(false));
        assert_eq!(folder.pack_format, Some(15));
        assert_eq!(folder.description.as_deref(), Some("目录形态"));
        assert!(folder.size_bytes > 0);

        // 列表里的 datapack_count 与之呼应。
        assert_eq!(svc.list(tmp.path()).unwrap()[0].datapack_count, 2);
    }

    #[test]
    fn datapack_without_mcmeta_still_lists() {
        let (tmp, svc) = manager();
        let saves = tmp.path().join("saves");
        let dir = make_save(&saves, "Plain", &Level::default());
        write_zip(
            &dir.join("datapacks").join("NoMeta.zip"),
            &[("data/x.txt", b"hi")],
        );

        let list = svc.datapacks(tmp.path(), "Plain").unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].enabled, None, "level.dat 没提到它");
        assert_eq!(list[0].pack_format, None);
        assert_eq!(list[0].description, None);
    }

    #[test]
    fn import_datapack_renames_on_conflict() {
        let (tmp, svc) = manager();
        let saves = tmp.path().join("saves");
        let dir = make_save(&saves, "Target", &Level::default());
        let source = tmp.path().join("MyPack.zip");
        std::fs::write(
            &source,
            zip_stored(&[("pack.mcmeta", br#"{"pack":{"pack_format":48}}"#)]),
        )
        .unwrap();

        assert_eq!(
            svc.import_datapack(tmp.path(), "Target", &source).unwrap(),
            "MyPack.zip"
        );
        assert_eq!(
            svc.import_datapack(tmp.path(), "Target", &source).unwrap(),
            "MyPack-1.zip"
        );
        assert_eq!(
            svc.import_datapack(tmp.path(), "Target", &source).unwrap(),
            "MyPack-2.zip"
        );
        for name in ["MyPack.zip", "MyPack-1.zip", "MyPack-2.zip"] {
            assert!(dir.join("datapacks").join(name).is_file(), "缺 {name}");
        }
        assert_eq!(svc.datapacks(tmp.path(), "Target").unwrap().len(), 3);
    }

    #[test]
    fn import_rejects_missing_and_non_zip_sources() {
        let (tmp, svc) = manager();
        let saves = tmp.path().join("saves");
        make_save(&saves, "Target", &Level::default());

        let missing = tmp.path().join("gone.zip");
        let err = svc
            .import_datapack(tmp.path(), "Target", &missing)
            .unwrap_err();
        assert!(matches!(err, ApiError::NotFound(_)), "{err:?}");

        let txt = tmp.path().join("notes.txt");
        std::fs::write(&txt, b"nope").unwrap();
        let err = svc.import_datapack(tmp.path(), "Target", &txt).unwrap_err();
        assert!(matches!(err, ApiError::Invalid(_)), "{err:?}");
    }

    #[test]
    fn delete_datapack_removes_zip_and_shrinks_list() {
        let (tmp, svc) = manager();
        let saves = tmp.path().join("saves");
        let dir = make_save(&saves, "Target", &Level::default());
        let packs = dir.join("datapacks");
        write_zip(&packs.join("Keep.zip"), &[("data/a.txt", b"a")]);
        write_zip(&packs.join("Drop.zip"), &[("data/b.txt", b"b")]);
        std::fs::create_dir_all(packs.join("DirPack")).unwrap();
        assert_eq!(svc.datapacks(tmp.path(), "Target").unwrap().len(), 3);

        svc.delete_datapack(tmp.path(), "Target", "Drop.zip")
            .unwrap();
        assert!(!packs.join("Drop.zip").exists());
        let left = svc.datapacks(tmp.path(), "Target").unwrap();
        assert_eq!(left.len(), 2);
        assert!(left.iter().all(|p| p.file_name != "Drop.zip"));

        // 目录形态的也支持删。
        svc.delete_datapack(tmp.path(), "Target", "DirPack")
            .unwrap();
        assert!(!packs.join("DirPack").exists());
        assert_eq!(svc.datapacks(tmp.path(), "Target").unwrap().len(), 1);

        let err = svc
            .delete_datapack(tmp.path(), "Target", "Drop.zip")
            .unwrap_err();
        assert!(matches!(err, ApiError::NotFound(_)), "{err:?}");
    }

    #[test]
    fn datapacks_of_unknown_save_is_not_found() {
        let (tmp, svc) = manager();
        std::fs::create_dir_all(tmp.path().join("saves")).unwrap();
        let err = svc.datapacks(tmp.path(), "Nope").unwrap_err();
        assert!(matches!(err, ApiError::NotFound(_)), "{err:?}");
    }

    // ---- 小工具 ------------------------------------------------------------

    #[test]
    fn pack_names_normalize_file_prefix_and_extension() {
        assert_eq!(normalize_pack_name("file/MyPack.zip"), "mypack");
        assert_eq!(normalize_pack_name("MyPack.ZIP"), "mypack");
        assert_eq!(normalize_pack_name(" dirpack "), "dirpack");
        assert_eq!(normalize_pack_name("vanilla"), "vanilla");
    }

    #[test]
    fn unique_name_keeps_extension() {
        assert_eq!(split_name("MyPack.zip"), ("MyPack", ".zip"));
        assert_eq!(split_name("noext"), ("noext", ""));

        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("A.zip"), b"x").unwrap();
        // 大小写不同也算撞名（Windows 文件系统就是这么判的）。
        assert_eq!(
            unique_datapack_name(tmp.path(), "a.ZIP").unwrap(),
            "a-1.ZIP"
        );
        assert_eq!(unique_datapack_name(tmp.path(), "B.zip").unwrap(), "B.zip");
    }
}
