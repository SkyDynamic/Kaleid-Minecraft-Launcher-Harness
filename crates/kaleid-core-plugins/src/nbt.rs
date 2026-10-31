//! 最小 NBT 读取器。
//!
//! 只为一件事存在：从 `level.dat` 里抠出存档名、游戏模式、最后游玩时间。
//! 引一个完整的 NBT 库不划算（多一个依赖、多一套 serde 约定），而 NBT 的
//! 二进制格式本身只有十几种标签，手写一遍比读文档还快。
//!
//! 安全取舍：
//! - **深度上限** `MAX_DEPTH`：损坏/恶意文件用嵌套把栈打穿。
//! - **解压上限** `MAX_BYTES`：gzip 炸弹。
//! - **列表长度上限** `MAX_LIST`：一个声称十亿项的 List 能把内存吃光。
//! - 字节数组/整型数组原样跳过（我们只要 `Data` 里的几个标量），
//!   否则 `Region` 之类的大数组会变成 JSON 里的天文数字字符串。

use std::io::Read;
use std::path::Path;

use kaleid_api::{ApiError, ApiResult};
use serde_json::{Map, Value};

/// 嵌套上限。
const MAX_DEPTH: u8 = 32;
/// 解压后最多读这么多字节。
const MAX_BYTES: u64 = 8 * 1024 * 1024;
/// 单个 List 最多取这么多元素。
const MAX_LIST: usize = 4096;

const TAG_END: u8 = 0;
const TAG_BYTE: u8 = 1;
const TAG_SHORT: u8 = 2;
const TAG_INT: u8 = 3;
const TAG_LONG: u8 = 4;
const TAG_FLOAT: u8 = 5;
const TAG_DOUBLE: u8 = 6;
const TAG_BYTE_ARRAY: u8 = 7;
const TAG_STRING: u8 = 8;
const TAG_LIST: u8 = 9;
const TAG_COMPOUND: u8 = 10;
const TAG_INT_ARRAY: u8 = 11;
const TAG_LONG_ARRAY: u8 = 12;

/// 读一个 `level.dat`（gzip 压缩的 NBT），返回根 Compound。
pub fn read_level_dat(path: &Path) -> ApiResult<Value> {
    let bytes =
        std::fs::read(path).map_err(|e| ApiError::io(format!("{}: {e}", path.display())))?;
    let raw = if bytes.starts_with(&[0x1f, 0x8b]) {
        let mut out = Vec::new();
        flate2::read::GzDecoder::new(bytes.as_slice())
            .take(MAX_BYTES)
            .read_to_end(&mut out)
            .map_err(|e| ApiError::Format(format!("{}: gzip 解压失败：{e}", path.display())))?;
        out
    } else {
        // 少数工具写出的是未压缩 NBT，一样能吃
        bytes
    };
    parse(&raw).map_err(|e| match e {
        ApiError::Format(msg) => ApiError::Format(format!("{}: {msg}", path.display())),
        other => other,
    })
}

/// 解析裸 NBT 字节流。
pub fn parse(bytes: &[u8]) -> ApiResult<Value> {
    let mut cur = Cursor { buf: bytes, pos: 0 };
    let tag = cur.u8()?;
    if tag != TAG_COMPOUND {
        return Err(ApiError::Format(format!("根标签不是 Compound（{tag}）")));
    }
    let _name = cur.string()?;
    let value = cur.compound(0)?;
    Ok(value)
}

/// 测试用：拼一个裸 NBT（未压缩）的 `level.dat`。
#[cfg(test)]
pub(crate) fn nbt_level_dat(
    name: &str,
    game_type: i32,
    last_played: i64,
    mc_version: &str,
) -> Vec<u8> {
    fn head(out: &mut Vec<u8>, tag: u8, name: &str) {
        out.push(tag);
        out.extend_from_slice(&(name.len() as u16).to_be_bytes());
        out.extend_from_slice(name.as_bytes());
    }
    fn body(out: &mut Vec<u8>, value: &str) {
        out.extend_from_slice(&(value.len() as u16).to_be_bytes());
        out.extend_from_slice(value.as_bytes());
    }
    let mut out = Vec::new();
    out.push(TAG_COMPOUND);
    out.extend_from_slice(&0u16.to_be_bytes()); // 根名字
    head(&mut out, TAG_COMPOUND, "Data");
    head(&mut out, TAG_STRING, "LevelName");
    body(&mut out, name);
    head(&mut out, TAG_INT, "GameType");
    out.extend_from_slice(&game_type.to_be_bytes());
    head(&mut out, TAG_LONG, "LastPlayed");
    out.extend_from_slice(&last_played.to_be_bytes());
    head(&mut out, TAG_COMPOUND, "Version");
    head(&mut out, TAG_STRING, "Name");
    body(&mut out, mc_version);
    out.push(TAG_END); // Version
    out.push(TAG_END); // Data
    out.push(TAG_END); // 根
    out
}

/// 测试用：gzip 压缩过的 `level.dat`。
#[cfg(test)]
pub(crate) fn level_dat_bytes(
    name: &str,
    game_type: i32,
    last_played: i64,
    mc_version: &str,
) -> Vec<u8> {
    use flate2::write::GzEncoder;
    use flate2::Compression;
    use std::io::Write;

    let mut enc = GzEncoder::new(Vec::new(), Compression::fast());
    enc.write_all(&nbt_level_dat(name, game_type, last_played, mc_version))
        .unwrap();
    enc.finish().unwrap()
}

struct Cursor<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn take(&mut self, n: usize) -> ApiResult<&'a [u8]> {
        let end = self
            .pos
            .checked_add(n)
            .ok_or_else(|| ApiError::Format("长度溢出".into()))?;
        if end > self.buf.len() {
            return Err(ApiError::Format(format!(
                "NBT 数据提前结束（偏移 {}，还要 {n} 字节）",
                self.pos
            )));
        }
        let slice = &self.buf[self.pos..end];
        self.pos = end;
        Ok(slice)
    }

    fn u8(&mut self) -> ApiResult<u8> {
        Ok(self.take(1)?[0])
    }

    fn be(&mut self, n: usize) -> ApiResult<u64> {
        let raw = self.take(n)?;
        let mut buf = [0u8; 8];
        buf[8 - n..].copy_from_slice(raw);
        Ok(u64::from_be_bytes(buf))
    }

    fn i16(&mut self) -> ApiResult<i16> {
        Ok(self.be(2)? as u16 as i16)
    }

    fn i32(&mut self) -> ApiResult<i32> {
        Ok(self.be(4)? as u32 as i32)
    }

    fn string(&mut self) -> ApiResult<String> {
        let len = self.be(2)? as usize;
        let raw = self.take(len)?;
        Ok(String::from_utf8_lossy(raw).into_owned())
    }

    fn payload(&mut self, tag: u8, depth: u8) -> ApiResult<Value> {
        if depth > MAX_DEPTH {
            return Err(ApiError::Format("NBT 嵌套过深".into()));
        }
        Ok(match tag {
            TAG_BYTE => Value::from(self.u8()? as i8),
            TAG_SHORT => Value::from(self.i16()?),
            TAG_INT => Value::from(self.i32()?),
            TAG_LONG => Value::from(self.be(8)? as i64),
            TAG_FLOAT => Value::from(f32::from_bits(self.be(4)? as u32) as f64),
            TAG_DOUBLE => Value::from(f64::from_bits(self.be(8)?)),
            TAG_BYTE_ARRAY => {
                let len = self.i32()?.max(0) as usize;
                self.take(len)?;
                Value::Null
            }
            TAG_STRING => Value::from(self.string()?),
            TAG_LIST => {
                let item = self.u8()?;
                let len = self.i32()?.max(0) as usize;
                if item == TAG_END || len == 0 {
                    return Ok(Value::Array(Vec::new()));
                }
                let keep = len.min(MAX_LIST);
                let mut out = Vec::with_capacity(keep);
                for i in 0..len {
                    let value = self.payload(item, depth + 1)?;
                    if i < keep {
                        out.push(value);
                    }
                }
                Value::Array(out)
            }
            TAG_COMPOUND => self.compound(depth + 1)?,
            TAG_INT_ARRAY => {
                let len = self.i32()?.max(0) as usize;
                self.take(len.saturating_mul(4))?;
                Value::Null
            }
            TAG_LONG_ARRAY => {
                let len = self.i32()?.max(0) as usize;
                self.take(len.saturating_mul(8))?;
                Value::Null
            }
            other => return Err(ApiError::Format(format!("未知 NBT 标签：{other}"))),
        })
    }

    fn compound(&mut self, depth: u8) -> ApiResult<Value> {
        if depth > MAX_DEPTH {
            return Err(ApiError::Format("NBT 嵌套过深".into()));
        }
        let mut map = Map::new();
        loop {
            let tag = self.u8()?;
            if tag == TAG_END {
                break;
            }
            let name = self.string()?;
            let value = self.payload(tag, depth + 1)?;
            map.insert(name, value);
        }
        Ok(Value::Object(map))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 拼一个 `Data` 里带 LevelName / GameType / LastPlayed 的 level.dat 字节流。
    fn sample() -> Vec<u8> {
        let mut out = Vec::new();
        out.push(TAG_COMPOUND);
        out.extend_from_slice(&0u16.to_be_bytes()); // 根名字为空
                                                    // Data: compound
        out.push(TAG_COMPOUND);
        out.extend_from_slice(&4u16.to_be_bytes());
        out.extend_from_slice(b"Data");
        // LevelName: string
        out.push(TAG_STRING);
        out.extend_from_slice(&9u16.to_be_bytes());
        out.extend_from_slice(b"LevelName");
        out.extend_from_slice(&5u16.to_be_bytes());
        out.extend_from_slice(b"World".as_slice());
        // GameType: int
        out.push(TAG_INT);
        out.extend_from_slice(&8u16.to_be_bytes());
        out.extend_from_slice(b"GameType");
        out.extend_from_slice(&1i32.to_be_bytes());
        // LastPlayed: long
        out.push(TAG_LONG);
        out.extend_from_slice(&10u16.to_be_bytes());
        out.extend_from_slice(b"LastPlayed");
        out.extend_from_slice(&1_700_000_000_000i64.to_be_bytes());
        // 字节数组：应被跳过
        out.push(TAG_BYTE_ARRAY);
        out.extend_from_slice(&3u16.to_be_bytes());
        out.extend_from_slice(b"Big");
        out.extend_from_slice(&4i32.to_be_bytes());
        out.extend_from_slice(&[1, 2, 3, 4]);
        // Data 结束
        out.push(TAG_END);
        // 根结束
        out.push(TAG_END);
        out
    }

    #[test]
    fn parses_scalars_and_skips_arrays() {
        let value = parse(&sample()).unwrap();
        let data = value.get("Data").unwrap();
        assert_eq!(data.get("LevelName").unwrap(), &Value::from("World"));
        assert_eq!(data.get("GameType").unwrap(), &Value::from(1));
        assert_eq!(
            data.get("LastPlayed").unwrap(),
            &Value::from(1_700_000_000_000i64)
        );
        assert_eq!(data.get("Big").unwrap(), &Value::Null);
    }

    #[test]
    fn truncated_input_is_a_format_error() {
        let full = sample();
        let err = parse(&full[..full.len() / 2]).unwrap_err();
        assert!(matches!(err, ApiError::Format(_)), "{err:?}");
    }

    #[test]
    fn rejects_non_compound_root() {
        let err = parse(&[TAG_INT, 0, 0, 0, 0, 0, 0]).unwrap_err();
        assert!(matches!(err, ApiError::Format(_)), "{err:?}");
    }

    #[test]
    fn gzip_level_dat_roundtrip() {
        use flate2::write::GzEncoder;
        use flate2::Compression;
        use std::io::Write;

        let mut enc = GzEncoder::new(Vec::new(), Compression::fast());
        enc.write_all(&sample()).unwrap();
        let gz = enc.finish().unwrap();

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("level.dat");
        std::fs::write(&path, &gz).unwrap();

        let value = read_level_dat(&path).unwrap();
        assert_eq!(
            value
                .get("Data")
                .and_then(|d| d.get("LevelName"))
                .and_then(|v| v.as_str()),
            Some("World")
        );
    }
}
