//! 极小的 JSON-Schema 子集校验器。
//!
//! 插件用它声明自己的配置长什么样，插件管理页据此自动生成表单。
//! 这里刻意只支持一个**封闭子集**（够用且可穷举测试），不引入 jsonschema 依赖：
//!
//! `type` / `properties` / `required` / `additionalProperties` / `items` /
//! `enum` / `const` / `minLength` / `maxLength` / `pattern` / `minimum` / `maximum`
//!
//! 不支持 `$ref` / `oneOf` / `anyOf` / `allOf` / `if-then`，遇到直接判为「未识别关键字」，
//! 避免插件写出我们无法保证校验语义的 schema。

use serde_json::Value;

/// 校验失败原因，带可读路径（例如 `config.proxy.port`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaError {
    pub path: String,
    pub message: String,
}

impl std::fmt::Display for SchemaError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.path.is_empty() {
            write!(f, "{}", self.message)
        } else {
            write!(f, "{}: {}", self.path, self.message)
        }
    }
}

const SUPPORTED: &[&str] = &[
    "type",
    "properties",
    "required",
    "additionalProperties",
    "items",
    "enum",
    "const",
    "minLength",
    "maxLength",
    "pattern",
    "minimum",
    "maximum",
    "title",
    "description",
    "default",
];

/// 用 `schema` 校验 `instance`。
pub fn validate(schema: &Value, instance: &Value) -> Result<(), SchemaError> {
    validate_at(schema, instance, "")
}

fn err(path: &str, message: impl Into<String>) -> SchemaError {
    SchemaError {
        path: path.to_string(),
        message: message.into(),
    }
}

fn join(path: &str, key: &str) -> String {
    if path.is_empty() {
        key.to_string()
    } else {
        format!("{path}.{key}")
    }
}

fn kind_of(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(n) if n.is_i64() || n.is_u64() => "integer",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn validate_at(schema: &Value, instance: &Value, path: &str) -> Result<(), SchemaError> {
    let obj = match schema {
        Value::Bool(true) => return Ok(()),
        Value::Bool(false) => return Err(err(path, "此处不允许任何值")),
        Value::Object(o) => o,
        _ => return Err(err(path, "schema 必须是对象或布尔值")),
    };

    for key in obj.keys() {
        if !SUPPORTED.contains(&key.as_str()) {
            return Err(err(
                path,
                format!("schema 使用了不支持的关关键字 `{key}`（封闭子集）"),
            ));
        }
    }

    if let Some(Value::String(t)) = obj.get("type") {
        let ok = match t.as_str() {
            "object" => instance.is_object(),
            "array" => instance.is_array(),
            "string" => instance.is_string(),
            "integer" => instance.is_i64() || instance.is_u64(),
            "number" => instance.is_number(),
            "boolean" => instance.is_boolean(),
            "null" => instance.is_null(),
            other => return Err(err(path, format!("未知 type `{other}`"))),
        };
        if !ok {
            return Err(err(
                path,
                format!("类型应为 `{t}`，实际是 `{}`", kind_of(instance)),
            ));
        }
    }

    if let Some(c) = obj.get("const") {
        if c != instance {
            return Err(err(path, format!("应恒等于 {c}")));
        }
    }

    if let Some(Value::Array(variants)) = obj.get("enum") {
        if !variants.iter().any(|v| v == instance) {
            return Err(err(
                path,
                format!("取值必须是 {variants:?} 之一，实际是 {instance}"),
            ));
        }
    }

    if let Some(s) = instance.as_str() {
        if let Some(min) = obj.get("minLength").and_then(|n| n.as_u64()) {
            if (s.chars().count() as u64) < min {
                return Err(err(path, format!("长度不得少于 {min}")));
            }
        }
        if let Some(max) = obj.get("maxLength").and_then(|n| n.as_u64()) {
            if (s.chars().count() as u64) > max {
                return Err(err(path, format!("长度不得超过 {max}")));
            }
        }
        if let Some(Value::String(p)) = obj.get("pattern") {
            let re = SimpleRegex::compile(p)
                .map_err(|e| err(path, format!("schema pattern 非法：{e}")))?;
            if !re.is_match(s) {
                return Err(err(path, format!("不符合模式 `{p}`")));
            }
        }
    }

    if let Some(n) = instance.as_f64() {
        if let Some(min) = obj.get("minimum").and_then(|v| v.as_f64()) {
            if n < min {
                return Err(err(path, format!("不得小于 {min}")));
            }
        }
        if let Some(max) = obj.get("maximum").and_then(|v| v.as_f64()) {
            if n > max {
                return Err(err(path, format!("不得大于 {max}")));
            }
        }
    }

    match instance {
        Value::Object(map) => {
            let props = obj.get("properties").and_then(|p| p.as_object());
            if let Some(required) = obj.get("required").and_then(|r| r.as_array()) {
                for r in required {
                    if let Some(key) = r.as_str() {
                        if !map.contains_key(key) {
                            return Err(err(path, format!("缺少必填字段 `{key}`")));
                        }
                    }
                }
            }
            if obj.get("additionalProperties") == Some(&Value::Bool(false)) {
                if let Some(props) = props {
                    for key in map.keys() {
                        if !props.contains_key(key) {
                            return Err(err(&join(path, key), "未在 schema 中声明的字段"));
                        }
                    }
                }
            }
            if let Some(props) = props {
                for (key, sub) in props {
                    if let Some(v) = map.get(key) {
                        validate_at(sub, v, &join(path, key))?;
                    }
                }
            }
        }
        Value::Array(items) => {
            if let Some(sub) = obj.get("items") {
                for (i, v) in items.iter().enumerate() {
                    validate_at(sub, v, &join(path, &i.to_string()))?;
                }
            }
        }
        _ => {}
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// 极简正则：只服务 schema pattern 的「防呆」用途，不是安全边界。
// 支持 `^` `$` `.` `[...]` `[^...]` `\d` `\w` `\s` 字面量与 `* + ? {n} {n,} {n,m}`。
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
enum Atom {
    Literal(char),
    Any,
    Class {
        negated: bool,
        ranges: Vec<(char, char)>,
    },
}

#[derive(Debug, Clone)]
struct Item {
    atom: Atom,
    min: usize,
    max: usize,
}

struct SimpleRegex {
    anchored_start: bool,
    anchored_end: bool,
    items: Vec<Item>,
}

impl SimpleRegex {
    fn compile(pattern: &str) -> Result<Self, String> {
        let mut chars: Vec<char> = pattern.chars().collect();
        let mut anchored_start = false;
        let mut anchored_end = false;
        if chars.first() == Some(&'^') {
            anchored_start = true;
            chars.remove(0);
        }
        if chars.last() == Some(&'$') {
            anchored_end = true;
            chars.pop();
        }

        let mut items: Vec<Item> = Vec::new();
        let mut i = 0usize;
        while i < chars.len() {
            let c = chars[i];
            let atom = match c {
                '.' => {
                    i += 1;
                    Atom::Any
                }
                '[' => {
                    let (atom, next) = parse_class(&chars, i)?;
                    i = next;
                    atom
                }
                '\\' => {
                    let n = *chars.get(i + 1).ok_or("模式以反斜杠结尾")?;
                    i += 2;
                    Atom::Class {
                        negated: false,
                        ranges: match n {
                            'd' => vec![('0', '9')],
                            'w' => vec![('a', 'z'), ('A', 'Z'), ('0', '9'), ('_', '_')],
                            's' => vec![(' ', ' '), ('\t', '\t')],
                            other => vec![(other, other)],
                        },
                    }
                }
                '(' | ')' | '|' => return Err(format!("暂不支持分组/分支 `{c}`")),
                '*' | '+' | '?' | '{' => return Err(format!("`{c}` 前缺少可重复的字符")),
                other => {
                    i += 1;
                    Atom::Literal(other)
                }
            };

            let (min, max) = match chars.get(i) {
                Some('*') => {
                    i += 1;
                    (0, usize::MAX)
                }
                Some('+') => {
                    i += 1;
                    (1, usize::MAX)
                }
                Some('?') => {
                    i += 1;
                    (0, 1)
                }
                Some('{') => {
                    let close = chars[i..]
                        .iter()
                        .position(|&ch| ch == '}')
                        .map(|p| p + i)
                        .ok_or("缺少 `}`")?;
                    let body: String = chars[i + 1..close].iter().collect();
                    let parsed = match body.split_once(',') {
                        Some((a, b)) => {
                            let lo = if a.trim().is_empty() {
                                0
                            } else {
                                a.trim().parse::<usize>().map_err(|_| "无效的 {n,m}")?
                            };
                            let hi = if b.trim().is_empty() {
                                usize::MAX
                            } else {
                                b.trim().parse::<usize>().map_err(|_| "无效的 {n,m}")?
                            };
                            (lo, hi)
                        }
                        None => {
                            let n = body.trim().parse::<usize>().map_err(|_| "无效的 {n}")?;
                            (n, n)
                        }
                    };
                    i = close + 1;
                    parsed
                }
                _ => (1, 1),
            };

            items.push(Item { atom, min, max });
        }

        Ok(SimpleRegex {
            anchored_start,
            anchored_end,
            items,
        })
    }

    fn is_match(&self, text: &str) -> bool {
        let chars: Vec<char> = text.chars().collect();
        if self.anchored_start {
            return self.match_from(&chars, 0, 0);
        }
        for start in 0..=chars.len() {
            if self.match_from(&chars, start, 0) {
                return true;
            }
        }
        false
    }

    fn match_from(&self, chars: &[char], pos: usize, idx: usize) -> bool {
        if idx == self.items.len() {
            return if self.anchored_end {
                pos == chars.len()
            } else {
                true
            };
        }
        let item = &self.items[idx];

        // 先尽量多吃，再从多到少回溯。
        let mut positions = vec![pos];
        let mut p = pos;
        let mut count = 0usize;
        while count < item.max {
            match match_atom(&item.atom, chars, p) {
                Some(np) => {
                    positions.push(np);
                    p = np;
                    count += 1;
                }
                None => break,
            }
        }

        let mut c = count;
        loop {
            if c >= item.min && self.match_from(chars, positions[c], idx + 1) {
                return true;
            }
            if c == 0 {
                return false;
            }
            c -= 1;
        }
    }
}

fn match_atom(atom: &Atom, chars: &[char], pos: usize) -> Option<usize> {
    let c = *chars.get(pos)?;
    match atom {
        Atom::Literal(l) if *l == c => Some(pos + 1),
        Atom::Any if c != '\n' => Some(pos + 1),
        Atom::Class { negated, ranges } => {
            let inside = ranges.iter().any(|(lo, hi)| c >= *lo && c <= *hi);
            if inside != *negated {
                Some(pos + 1)
            } else {
                None
            }
        }
        _ => None,
    }
}

fn parse_class(chars: &[char], start: usize) -> Result<(Atom, usize), String> {
    let mut i = start + 1;
    let mut negated = false;
    if chars.get(i) == Some(&'^') {
        negated = true;
        i += 1;
    }
    let mut ranges = Vec::new();
    while let Some(&c) = chars.get(i) {
        if c == ']' {
            if ranges.is_empty() {
                return Err("空的字符类 `[]`".into());
            }
            return Ok((Atom::Class { negated, ranges }, i + 1));
        }
        if chars.get(i + 1) == Some(&'-') && chars.get(i + 2).is_some_and(|n| *n != ']') {
            let hi = chars[i + 2];
            ranges.push((c, hi));
            i += 3;
        } else {
            ranges.push((c, c));
            i += 1;
        }
    }
    Err("字符类缺少 `]`".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn type_mismatch_reports_path() {
        let schema = json!({
            "type": "object",
            "properties": { "port": { "type": "integer" } }
        });
        let e = validate(&schema, &json!({ "port": "8080" })).unwrap_err();
        assert_eq!(e.path, "port");
    }

    #[test]
    fn required_and_additional_properties() {
        let schema = json!({
            "type": "object",
            "required": ["mirror"],
            "additionalProperties": false,
            "properties": { "mirror": { "type": "string" } }
        });
        assert!(validate(&schema, &json!({})).is_err());
        assert!(validate(&schema, &json!({ "mirror": "a", "typo": 1 })).is_err());
        assert!(validate(&schema, &json!({ "mirror": "a" })).is_ok());
    }

    #[test]
    fn enum_and_const() {
        let schema = json!({ "enum": ["auto", "mojang", "bmclapi"] });
        assert!(validate(&schema, &json!("auto")).is_ok());
        assert!(validate(&schema, &json!("other")).is_err());
    }

    #[test]
    fn pattern_for_offline_player_name() {
        let schema = json!({
            "type": "string",
            "minLength": 1,
            "maxLength": 16,
            "pattern": "^[A-Za-z0-9_]+$"
        });
        assert!(validate(&schema, &json!("Player_123")).is_ok());
        assert!(validate(&schema, &json!("")).is_err());
        assert!(validate(&schema, &json!("玩家名")).is_err());
        assert!(validate(&schema, &json!("bad name")).is_err());
        assert!(validate(&schema, &json!("a".repeat(17))).is_err());
        assert!(validate(&schema, &json!("a".repeat(16))).is_ok());
    }

    #[test]
    fn rejects_unsupported_keywords() {
        let schema = json!({ "oneOf": [ { "type": "string" } ] });
        let e = validate(&schema, &json!("x")).unwrap_err();
        assert!(e.message.contains("不支持的关关键字"));
    }

    #[test]
    fn numeric_bounds() {
        let schema = json!({ "type": "integer", "minimum": 512, "maximum": 32768 });
        assert!(validate(&schema, &json!(1024)).is_ok());
        assert!(validate(&schema, &json!(1)).is_err());
        assert!(validate(&schema, &json!(99999)).is_err());
    }

    #[test]
    fn array_items() {
        let schema = json!({ "type": "array", "items": { "type": "string" } });
        assert!(validate(&schema, &json!(["a", "b"])).is_ok());
        assert!(validate(&schema, &json!(["a", 1])).is_err());
        assert_eq!(validate(&schema, &json!(["a", 1])).unwrap_err().path, "1");
    }

    #[test]
    fn regex_engine_behaviour() {
        let re = SimpleRegex::compile("^[A-Za-z0-9_]+$").unwrap();
        assert!(re.is_match("abc_123"));
        assert!(!re.is_match("abc-123"));
        assert!(!re.is_match(""));
        assert!(!re.is_match("abc 123"));

        let re2 = SimpleRegex::compile("^v\\d+\\.\\d+$").unwrap();
        assert!(re2.is_match("v1.21"));
        assert!(!re2.is_match("v1.21.4"));

        let re3 = SimpleRegex::compile("^fabric-loader-.*$").unwrap();
        assert!(re3.is_match("fabric-loader-0.16.9-1.21.4"));
        assert!(!re3.is_match("forge-1.21.4"));

        let re4 = SimpleRegex::compile("^a{2,4}$").unwrap();
        assert!(re4.is_match("aa"));
        assert!(re4.is_match("aaaa"));
        assert!(!re4.is_match("a"));
        assert!(!re4.is_match("aaaaa"));
    }
}
