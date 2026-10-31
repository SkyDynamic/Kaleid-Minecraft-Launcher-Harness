//! CurseForge 文件指纹。
//!
//! CurseForge 不用 SHA1 认文件，而是按**归一化后的内容**算一个 32 位
//! MurmurHash2（seed = 1）。所以要判断"磁盘上这个 jar 是不是 CurseForge 上那个
//! 文件"，必须连归一化规则一起照抄：哈希算法或归一化差一步，指纹就对不上，
//! 结果就是同一版本的模组在更新检查里永远显示"来源未知"。
//!
//! 归一化只有两条：
//! 1. 丢掉所有空白字节（`0x09` / `0x0a` / `0x0d` / `0x20`）—— 同一个文件被不同
//!    工具重新打包、或换行风格不同时，只有这些字节会变；
//! 2. 末尾若带 CurseForge 那 9 字节标记，先切掉再算，否则指纹会固定差一截。

use std::path::Path;

/// CurseForge 归一化时会切掉的 9 字节尾巴。
const CF_TAIL: [u8; 9] = [0x01, 0x01, 0x00, 0x2c, 0x00, 0x00, 0x00, 0x00, 0x01];

/// CurseForge 指纹：先按官方规则归一化文件内容，再算 MurmurHash2。
pub fn curseforge_fingerprint(bytes: &[u8]) -> u32 {
    murmur2(&normalize_for_fingerprint(bytes), 1)
}

/// 官方归一化：去掉所有空白字节（0x09、0x0a、0x0d、0x20），
/// 再在末尾出现 CurseForge 的 9 字节标记时把它去掉。
pub fn normalize_for_fingerprint(bytes: &[u8]) -> Vec<u8> {
    let mut out: Vec<u8> = bytes
        .iter()
        .copied()
        .filter(|byte| !is_ignored_byte(*byte))
        .collect();
    // 顺序不能反：先按字节去空白再切尾巴，标记本身不含空白，两种顺序结果一样，
    // 但按"官方先归一化再切"来写更好核对。
    if out.ends_with(&CF_TAIL) {
        out.truncate(out.len() - CF_TAIL.len());
    }
    out
}

/// 便捷入口：直接对磁盘文件算指纹（读取失败返回 None）。
pub fn fingerprint_file(path: &Path) -> Option<u32> {
    // 一次性读全：归一化要看到文件末尾的标记，MurmurHash2 又要先知道总长度，
    // 分块流式的版本省不下多少内存，却更难核对正确性。
    let bytes = std::fs::read(path).ok()?;
    Some(curseforge_fingerprint(&bytes))
}

/// 这些字节在"同一份文件换个工具重打"时会变，CurseForge 因此不看它们。
fn is_ignored_byte(byte: u8) -> bool {
    matches!(byte, 0x09 | 0x0a | 0x0d | 0x20)
}

/// MurmurHash2 的 32 位版本（Appleby 的参考实现，seed = 1 是 CurseForge 用的）。
///
/// 常量与步骤一律照抄：`m`、`r`、整块按小端取，剩下的 1~3 个字节按低字节在前
/// 拼进去，最后再做 `>>13 / *m / >>15` 三步混合。少一步、换一种字节序，
/// 算出来的值就和 CurseForge 上的对不上。
fn murmur2(data: &[u8], seed: u32) -> u32 {
    const M: u32 = 0x5bd1_e995;
    const R: u32 = 24;

    // 长度按 u32 参与初始化（参考实现里是 int）。>4 GiB 的文件不在本场景里。
    let mut h = seed ^ (data.len() as u32);

    let blocks = data.len() / 4;
    for index in 0..blocks {
        let at = index * 4;
        let mut k = u32::from_le_bytes([data[at], data[at + 1], data[at + 2], data[at + 3]]);
        k = k.wrapping_mul(M);
        k ^= k >> R;
        k = k.wrapping_mul(M);

        h = h.wrapping_mul(M);
        h ^= k;
    }

    let tail = &data[blocks * 4..];
    if let Some(byte) = tail.get(2) {
        h ^= u32::from(*byte) << 16;
    }
    if let Some(byte) = tail.get(1) {
        h ^= u32::from(*byte) << 8;
    }
    if let Some(byte) = tail.first() {
        h ^= u32::from(*byte);
        h = h.wrapping_mul(M);
    }

    h ^= h >> 13;
    h = h.wrapping_mul(M);
    h ^= h >> 15;
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn murmur2_matches_reference_vectors() {
        // 期望值来自 Appleby 参考实现的独立复算（seed = 1）。空串不是 0：
        // 初始化 `h = seed ^ len` 之后仍要走完最后三步混合。
        assert_eq!(murmur2(b"", 1), 0x5bd1_5e36);
        // 只有一个尾字节
        assert_eq!(murmur2(b"a", 1), 0x2550_b18c);
        // 三个尾字节（高位在前拼）
        assert_eq!(murmur2(b"abc", 1), 0x60a4_fcc1);
        // 正好一块，没有尾字节
        assert_eq!(murmur2(b"abcd", 1), 0xc93f_7a16);
        // 两块，同样没有尾字节
        assert_eq!(murmur2(b"abcdefgh", 1), 0x4065_991a);
        // 高位字节（0x80/0xff）：查移位时有没有被当成有符号数
        assert_eq!(murmur2(&[0x00, 0xff, 0x80, 0x7f], 1), 0xace0_8d6d);
    }

    #[test]
    fn whitespace_bytes_are_dropped() {
        // 换行风格、行尾空格、Tab 不同，CurseForge 视为同一个文件
        let messy = b"a b\r\n\tc";
        assert_eq!(normalize_for_fingerprint(messy), b"abc".to_vec());
        assert_eq!(curseforge_fingerprint(messy), 0x60a4_fcc1);
        assert_eq!(
            curseforge_fingerprint(messy),
            curseforge_fingerprint(b"abc")
        );

        // 全是空白 → 归一化后为空，指纹等于 murmur2(b"", 1)
        assert!(normalize_for_fingerprint(b" \t\r\n").is_empty());
        assert_eq!(curseforge_fingerprint(b" \t\r\n"), murmur2(b"", 1));
    }

    #[test]
    fn trailing_marker_is_stripped() {
        let marker = [0x01, 0x01, 0x00, 0x2c, 0x00, 0x00, 0x00, 0x00, 0x01];

        let mut with_marker = b"abc".to_vec();
        with_marker.extend_from_slice(&marker);
        assert_eq!(normalize_for_fingerprint(&with_marker), b"abc".to_vec());
        assert_eq!(
            curseforge_fingerprint(&with_marker),
            curseforge_fingerprint(b"abc")
        );

        // 只切末尾那一整段：中间出现同样的字节不算标记
        let mut middle = b"abc".to_vec();
        middle.extend_from_slice(&marker);
        middle.push(b'z');
        assert_eq!(
            normalize_for_fingerprint(&middle),
            middle.clone(),
            "标记不在末尾时一个字节都不该动"
        );
        assert_ne!(
            curseforge_fingerprint(&middle),
            curseforge_fingerprint(b"abc")
        );
    }

    #[test]
    fn fingerprint_file_matches_memory_version() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mod.jar");
        std::fs::write(&path, b"a b\nc").unwrap();

        assert_eq!(fingerprint_file(&path), Some(0x60a4_fcc1));
        assert_eq!(
            fingerprint_file(&path),
            Some(curseforge_fingerprint(b"a b\nc"))
        );
        // 读不到就是 None，不猜一个值出来
        assert_eq!(fingerprint_file(&dir.path().join("missing.jar")), None);
    }
}
