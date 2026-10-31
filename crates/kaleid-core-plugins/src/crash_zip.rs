//! 排障包 ZIP 写入器。
//!
//! 只为了「把几个文件打成一个包」引入一个几百 KB 的 zip 依赖不划算，这里直接写
//! ZIP 规范里最小的那个子集：deflate（方法 8）+ 中央目录 + 结束记录。生成的包
//! 资源管理器、7-Zip、`Expand-Archive` 都能直接打开。
//!
//! 文件名里的时间用**本地时间**：用户拿到 `崩溃日志-2026_10_31 20-45-01.zip`
//! 时要一眼对得上自己几点崩的，UTC 会差八小时。Windows 文件名不允许 `:`，
//! 所以时分秒之间用 `-`。

use std::io::Write;
use std::path::{Path, PathBuf};

use kaleid_api::{ApiError, ApiResult};

/// 一个待写入的条目。
pub struct ZipEntry {
    pub name: String,
    pub bytes: Vec<u8>,
}

impl ZipEntry {
    pub fn new(name: impl Into<String>, bytes: impl Into<Vec<u8>>) -> Self {
        ZipEntry {
            name: name.into(),
            bytes: bytes.into(),
        }
    }
}

/// 打包结果。
#[derive(Debug, Clone)]
pub struct ZipOutcome {
    pub path: PathBuf,
    pub bytes: u64,
    pub files: usize,
}

/// 本地时间的一份快照（打包文件名用）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalNow {
    pub year: u32,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
}

impl LocalNow {
    /// `崩溃日志-2026_10_31 20-45-01.zip`
    pub fn archive_name(&self) -> String {
        format!(
            "崩溃日志-{:04}_{:02}_{:02} {:02}-{:02}-{:02}.zip",
            self.year, self.month, self.day, self.hour, self.minute, self.second
        )
    }

    /// MS-DOS 时间戳：`time = h<<11 | m<<5 | s/2`，`date = (year-1980)<<9 | month<<5 | day`。
    fn dos_stamp(&self) -> (u16, u16) {
        let time = ((self.hour.min(23) << 11)
            | (self.minute.min(59) << 5)
            | (self.second.min(59) / 2)) as u16;
        let year = self.year.max(1980) - 1980;
        let date = ((year << 9) | (self.month.clamp(1, 12) << 5) | self.day.clamp(1, 31)) as u16;
        (time, date)
    }
}

/// 当前本地时间。
#[cfg(windows)]
pub fn local_now() -> LocalNow {
    use windows_sys::Win32::System::SystemInformation::GetLocalTime;
    // SAFETY: 只是往栈上填一个定长结构体，没有所有权问题。
    let mut stamp: windows_sys::Win32::Foundation::SYSTEMTIME = unsafe { std::mem::zeroed() };
    unsafe { GetLocalTime(&mut stamp) };
    LocalNow {
        year: stamp.wYear as u32,
        month: stamp.wMonth as u32,
        day: stamp.wDay as u32,
        hour: stamp.wHour as u32,
        minute: stamp.wMinute as u32,
        second: stamp.wSecond as u32,
    }
}

/// 其它平台退回 UTC（启动器主要面是 Windows，这里只保证能编译、能跑）。
#[cfg(not(windows))]
pub fn local_now() -> LocalNow {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let (year, month, day) = civil_from_days(days);
    LocalNow {
        year: year as u32,
        month: month as u32,
        day: day as u32,
        hour: (rem / 3600) as u32,
        minute: ((rem % 3600) / 60) as u32,
        second: (rem % 60) as u32,
    }
}

/// 天 → 公历日期（Howard Hinnant 的 `civil_from_days`）。
#[cfg(not(windows))]
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = if m <= 2 { y + 1 } else { y };
    (year, m, d)
}

/// 把一个目录里的文件全部打进 ZIP。
pub fn zip_directory(target_dir: &Path, name: &str, dir: &Path) -> ApiResult<ZipOutcome> {
    let mut entries: Vec<ZipEntry> = Vec::new();
    let reader = std::fs::read_dir(dir)
        .map_err(|e| ApiError::io(format!("读取导出目录失败：{e}")))?;
    for entry in reader.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(file_name) = path.file_name().map(|s| s.to_string_lossy().to_string()) else {
            continue;
        };
        let bytes = std::fs::read(&path)
            .map_err(|e| ApiError::io(format!("读取 {} 失败：{e}", path.display())))?;
        entries.push(ZipEntry::new(file_name, bytes));
    }
    if entries.is_empty() {
        return Err(ApiError::invalid("没有可打包的文件"));
    }
    // 名字排一下序：解压出来的顺序稳定，人肉核对时不容易漏。
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    write_zip(target_dir, name, &entries)
}

/// 写一个 ZIP 到 `target_dir/name`。
pub fn write_zip(target_dir: &Path, name: &str, entries: &[ZipEntry]) -> ApiResult<ZipOutcome> {
    std::fs::create_dir_all(target_dir)
        .map_err(|e| ApiError::io(format!("创建导出目录失败：{e}")))?;
    let path = target_dir.join(name);
    let bytes = build_zip(entries, local_now())?;
    std::fs::write(&path, &bytes).map_err(|e| ApiError::io(format!("写入压缩包失败：{e}")))?;
    Ok(ZipOutcome {
        path,
        bytes: bytes.len() as u64,
        files: entries.len(),
    })
}

/// 真正的字节拼装（单独拆出来，测试里不用碰磁盘）。
pub fn build_zip(entries: &[ZipEntry], stamp: LocalNow) -> ApiResult<Vec<u8>> {
    let (dos_time, dos_date) = stamp.dos_stamp();
    let mut out: Vec<u8> = Vec::new();
    let mut central: Vec<u8> = Vec::new();
    let mut written_names = std::collections::BTreeSet::new();

    for entry in entries {
        // 条目名必须是相对路径、不带盘符；重复的条目会让解压器行为不可预期。
        let name = entry.name.replace('\\', "/");
        if name.is_empty() || name.starts_with('/') || !written_names.insert(name.clone()) {
            continue;
        }
        let compressed = deflate(&entry.bytes)?;
        let crc = crc32(&entry.bytes);
        let offset = out.len() as u32;

        // ---- 本地文件头 ----
        out.extend_from_slice(&0x0403_4b50u32.to_le_bytes()); // 签名
        out.extend_from_slice(&20u16.to_le_bytes()); // 解压需要 2.0
        out.extend_from_slice(&0u16.to_le_bytes()); // 通用标志
        out.extend_from_slice(&8u16.to_le_bytes()); // deflate
        out.extend_from_slice(&dos_time.to_le_bytes());
        out.extend_from_slice(&dos_date.to_le_bytes());
        out.extend_from_slice(&crc.to_le_bytes());
        out.extend_from_slice(&(compressed.len() as u32).to_le_bytes());
        out.extend_from_slice(&(entry.bytes.len() as u32).to_le_bytes());
        out.extend_from_slice(&(name.len() as u16).to_le_bytes()); // 文件名长度
        out.extend_from_slice(&0u16.to_le_bytes()); // extra 长度
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(&compressed);

        // ---- 中央目录条目 ----
        central.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
        central.extend_from_slice(&20u16.to_le_bytes()); // 打包者版本
        central.extend_from_slice(&20u16.to_le_bytes()); // 解压需要
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&8u16.to_le_bytes());
        central.extend_from_slice(&dos_time.to_le_bytes());
        central.extend_from_slice(&dos_date.to_le_bytes());
        central.extend_from_slice(&crc.to_le_bytes());
        central.extend_from_slice(&(compressed.len() as u32).to_le_bytes());
        central.extend_from_slice(&(entry.bytes.len() as u32).to_le_bytes());
        central.extend_from_slice(&(name.len() as u16).to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes()); // extra
        central.extend_from_slice(&0u16.to_le_bytes()); // 注释
        central.extend_from_slice(&0u16.to_le_bytes()); // 磁盘号
        central.extend_from_slice(&0u16.to_le_bytes()); // 内部属性
        central.extend_from_slice(&0u32.to_le_bytes()); // 外部属性
        central.extend_from_slice(&offset.to_le_bytes());
        central.extend_from_slice(name.as_bytes());
    }

    let central_offset = out.len() as u32;
    let central_size = central.len() as u32;
    out.extend_from_slice(&central);

    // ---- 中央目录结束记录 ----
    out.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes()); // 本磁盘号
    out.extend_from_slice(&0u16.to_le_bytes()); // 中央目录起始磁盘
    out.extend_from_slice(&(written_names.len() as u16).to_le_bytes());
    out.extend_from_slice(&(written_names.len() as u16).to_le_bytes());
    out.extend_from_slice(&central_size.to_le_bytes());
    out.extend_from_slice(&central_offset.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes()); // 注释长度

    Ok(out)
}

fn deflate(bytes: &[u8]) -> ApiResult<Vec<u8>> {
    let mut encoder = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
    encoder
        .write_all(bytes)
        .map_err(|e| ApiError::io(format!("压缩失败：{e}")))?;
    encoder
        .finish()
        .map_err(|e| ApiError::io(format!("压缩失败：{e}")))
}

/// ZIP 用的 CRC-32（和 `flate2::Crc` 是同一个算法，这里自己算是为了不依赖内部类型）。
fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for byte in bytes {
        crc ^= *byte as u32;
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xedb8_8320 & mask);
        }
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::*;

    const STAMP: LocalNow = LocalNow {
        year: 2026,
        month: 10,
        day: 31,
        hour: 20,
        minute: 45,
        second: 1,
    };

    #[test]
    fn archive_name_has_no_colon() {
        let name = STAMP.archive_name();
        assert_eq!(name, "崩溃日志-2026_10_31 20-45-01.zip");
        assert!(!name.contains(':'), "Windows 文件名不能含冒号");
    }

    #[test]
    fn crc32_matches_known_value() {
        // "123456789" 的 CRC-32 是 0xCBF43926，这是 ZIP/PNG 用的同一个多项式。
        assert_eq!(crc32(b"123456789"), 0xcbf4_3926);
    }

    /// 写出来的包必须能被**独立**读回来：签名、条目数、文件名、内容都对。
    #[test]
    fn zip_round_trips_names_and_bytes() {
        let entries = vec![
            ZipEntry::new("latest.log", b"hello minecraft\n".to_vec()),
            ZipEntry::new("manifest.json", b"{\"a\":1}".to_vec()),
        ];
        let bytes = build_zip(&entries, STAMP).unwrap();
        assert_eq!(&bytes[..4], b"PK\x03\x04");

        // 从尾部找中央目录偏移
        let eocd = bytes.len() - 22;
        assert_eq!(&bytes[eocd..eocd + 4], b"PK\x05\x06");
        let count = u16::from_le_bytes([bytes[eocd + 10], bytes[eocd + 11]]);
        assert_eq!(count, 2);
        let central_offset = u32::from_le_bytes([
            bytes[eocd + 16],
            bytes[eocd + 17],
            bytes[eocd + 18],
            bytes[eocd + 19],
        ]) as usize;
        assert_eq!(&bytes[central_offset..central_offset + 4], b"PK\x01\x02");

        // 逐个条目检查：名字一致、解压后内容一致
        let mut cursor = central_offset;
        let mut seen = Vec::new();
        for _ in 0..count {
            let compressed_size = u32::from_le_bytes([
                bytes[cursor + 20],
                bytes[cursor + 21],
                bytes[cursor + 22],
                bytes[cursor + 23],
            ]) as usize;
            let name_len = u16::from_le_bytes([bytes[cursor + 28], bytes[cursor + 29]]) as usize;
            let extra_len = u16::from_le_bytes([bytes[cursor + 30], bytes[cursor + 31]]) as usize;
            let comment_len = u16::from_le_bytes([bytes[cursor + 32], bytes[cursor + 33]]) as usize;
            let local_offset = u32::from_le_bytes([
                bytes[cursor + 42],
                bytes[cursor + 43],
                bytes[cursor + 44],
                bytes[cursor + 45],
            ]) as usize;
            let name = String::from_utf8(
                bytes[cursor + 46..cursor + 46 + name_len].to_vec(),
            )
            .unwrap();

            let local_name_len =
                u16::from_le_bytes([bytes[local_offset + 26], bytes[local_offset + 27]]) as usize;
            let local_extra_len =
                u16::from_le_bytes([bytes[local_offset + 28], bytes[local_offset + 29]]) as usize;
            let data_start = local_offset + 30 + local_name_len + local_extra_len;
            let compressed = &bytes[data_start..data_start + compressed_size];
            let mut plain = Vec::new();
            let mut decoder = flate2::read::DeflateDecoder::new(compressed);
            std::io::Read::read_to_end(&mut decoder, &mut plain).unwrap();
            seen.push((name, plain));

            cursor += 46 + name_len + extra_len + comment_len;
        }
        seen.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(seen[0].0, "latest.log");
        assert_eq!(seen[0].1, b"hello minecraft\n");
        assert_eq!(seen[1].0, "manifest.json");
        assert_eq!(seen[1].1, b"{\"a\":1}");
    }

    #[test]
    fn writes_a_file_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        let out = write_zip(
            dir.path(),
            &STAMP.archive_name(),
            &[ZipEntry::new("a.txt", b"x".to_vec())],
        )
        .unwrap();
        assert!(out.path.is_file());
        assert_eq!(out.files, 1);
        assert!(out.bytes > 30);
    }
}
