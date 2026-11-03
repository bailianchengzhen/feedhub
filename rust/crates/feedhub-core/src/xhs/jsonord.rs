//! **保序 JSON**（自己写的一个小实现，只给小红书关注流的存盘格式用）。
//!
//! 为什么要另写一个：关注流的作者缓冲要**「Rust 读进来、再写出去和 Kotlin 逐字节一样」**，
//! 而 `serde_json` 的 `Map` 默认是 `BTreeMap`（按键名排序）——`FeedItem.emotes` 和 `states` 的顺序
//! 都是**插入顺序**（Kotlin 那边是 `LinkedHashMap` / `list`），一排序就"读进来再写出去"对不上。
//!
//! 为什么不用 `serde_json` 的 `preserve_order` 特性：那是**全局**改 `Map` 的类型（换 `IndexMap`），
//! 别的 11 家平台的解析都会跟着换成"按文档顺序遍历"，当前是纯新增、不动别家。
//!
//! 数字**保留原始字面量**（和 `serde_json` 的 `arbitrary_precision` 一样）：写回去逐字节不变。
//!
//! 只实现存盘格式需要的那一小撮：对象 / 数组 / 字符串 / 数字 / 布尔 / null，没有注释、没有 NaN。

/// 一个 JSON 值。对象和数组都**保持文本里的顺序**。
#[derive(Debug, Clone, PartialEq)]
pub enum J {
    Null,
    Bool(bool),
    /// 数字的**原始字面量**（`"1.7921573109610028E9"` 这种也原样留着）。
    Num(String),
    Str(String),
    Arr(Vec<J>),
    Obj(Vec<(String, J)>),
}

impl J {
    /// 对象里按名字取值（**先出现的为准**，和 serde 一样）。
    pub fn get(&self, key: &str) -> Option<&J> {
        match self {
            J::Obj(fields) => fields.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            J::Str(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            J::Bool(b) => Some(*b),
            _ => None,
        }
    }

    /// 整数：数字字面量（`1.9` 截断，和 Kotlin 的 `toLong()` 一样）或数字字符串。
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            J::Num(raw) => raw
                .parse::<i64>()
                .ok()
                .or_else(|| raw.parse::<f64>().ok().map(|f| f as i64)),
            J::Str(s) => s
                .parse::<i64>()
                .ok()
                .or_else(|| s.parse::<f64>().ok().map(|f| f as i64)),
            _ => None,
        }
    }

    /// 浮点：数字字面量按原样解析（**不截断**，和 [`Self::as_i64`] 不同）；数字字符串也认。
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            J::Num(raw) => raw.parse::<f64>().ok(),
            J::Str(s) => s.parse::<f64>().ok(),
            _ => None,
        }
    }

    pub fn as_arr(&self) -> Option<&[J]> {
        match self {
            J::Arr(v) => Some(v),
            _ => None,
        }
    }

    pub fn as_obj(&self) -> Option<&[(String, J)]> {
        match self {
            J::Obj(v) => Some(v),
            _ => None,
        }
    }
}

/// 解析一段 JSON 文本。失败只回一句人话（调用方一律当成"缓存坏了"，不往外抛）。
pub fn parse(text: &str) -> Result<J, String> {
    let bytes = text.as_bytes();
    let mut p = Parser { text, bytes, i: 0 };
    p.skip_ws();
    let value = p.value()?;
    p.skip_ws();
    if p.i != bytes.len() {
        return Err(format!("JSON 后面还有内容（第 {} 字节）", p.i));
    }
    Ok(value)
}

struct Parser<'a> {
    /// 原文（已经是合法 UTF-8）：字符串里的普通字符整段从这里切，不再逐字符校验。
    text: &'a str,
    bytes: &'a [u8],
    i: usize,
}

impl<'a> Parser<'a> {
    fn skip_ws(&mut self) {
        while self.i < self.bytes.len() {
            match self.bytes[self.i] {
                b' ' | b'\t' | b'\n' | b'\r' => self.i += 1,
                _ => break,
            }
        }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.i).copied()
    }

    fn value(&mut self) -> Result<J, String> {
        self.skip_ws();
        match self.peek() {
            None => Err("JSON 提前结束".to_string()),
            Some(b'{') => self.object(),
            Some(b'[') => self.array(),
            Some(b'"') => self.string().map(J::Str),
            Some(b't') => self.literal("true", J::Bool(true)),
            Some(b'f') => self.literal("false", J::Bool(false)),
            Some(b'n') => self.literal("null", J::Null),
            Some(c) if c == b'-' || c.is_ascii_digit() => self.number(),
            Some(c) => Err(format!("不认识的字符 '{}'（第 {} 字节）", c as char, self.i)),
        }
    }

    fn literal(&mut self, word: &str, value: J) -> Result<J, String> {
        if self.bytes[self.i..].starts_with(word.as_bytes()) {
            self.i += word.len();
            Ok(value)
        } else {
            Err(format!("第 {} 字节不是 {}", self.i, word))
        }
    }

    fn object(&mut self) -> Result<J, String> {
        self.i += 1; // '{'
        let mut out: Vec<(String, J)> = Vec::new();
        self.skip_ws();
        if self.peek() == Some(b'}') {
            self.i += 1;
            return Ok(J::Obj(out));
        }
        loop {
            self.skip_ws();
            let key = self.string()?;
            self.skip_ws();
            if self.peek() != Some(b':') {
                return Err(format!("第 {} 字节缺少 ':'", self.i));
            }
            self.i += 1;
            let value = self.value()?;
            out.push((key, value));
            self.skip_ws();
            match self.peek() {
                Some(b',') => self.i += 1,
                Some(b'}') => {
                    self.i += 1;
                    return Ok(J::Obj(out));
                }
                _ => return Err(format!("第 {} 字节的对象没有收尾", self.i)),
            }
        }
    }

    fn array(&mut self) -> Result<J, String> {
        self.i += 1; // '['
        let mut out: Vec<J> = Vec::new();
        self.skip_ws();
        if self.peek() == Some(b']') {
            self.i += 1;
            return Ok(J::Arr(out));
        }
        loop {
            out.push(self.value()?);
            self.skip_ws();
            match self.peek() {
                Some(b',') => self.i += 1,
                Some(b']') => {
                    self.i += 1;
                    return Ok(J::Arr(out));
                }
                _ => return Err(format!("第 {} 字节的数组没有收尾", self.i)),
            }
        }
    }

    fn string(&mut self) -> Result<String, String> {
        if self.peek() != Some(b'"') {
            return Err(format!("第 {} 字节不是字符串", self.i));
        }
        self.i += 1;
        let mut out = String::new();
        loop {
            let c = self.peek().ok_or_else(|| "字符串没有收尾".to_string())?;
            match c {
                b'"' => {
                    self.i += 1;
                    return Ok(out);
                }
                b'\\' => {
                    self.i += 1;
                    let esc = self.peek().ok_or_else(|| "转义没有收尾".to_string())?;
                    self.i += 1;
                    match esc {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{8}'),
                        b'f' => out.push('\u{c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => {
                            let hi = self.hex4()?;
                            if (0xD800..0xDC00).contains(&hi) {
                                // 代理对：后面紧跟 `\uDC00`–`\uDFFF` 才合成一个字符
                                if self.bytes[self.i..].starts_with(b"\\u") {
                                    let save = self.i;
                                    self.i += 2;
                                    let lo = self.hex4()?;
                                    if (0xDC00..0xE000).contains(&lo) {
                                        let code = 0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00);
                                        out.push(char::from_u32(code).unwrap_or('\u{fffd}'));
                                    } else {
                                        self.i = save;
                                        out.push(char::from_u32(hi).unwrap_or('\u{fffd}'));
                                    }
                                } else {
                                    out.push(char::from_u32(hi).unwrap_or('\u{fffd}'));
                                }
                            } else {
                                out.push(char::from_u32(hi).unwrap_or('\u{fffd}'));
                            }
                        }
                        other => return Err(format!("不认识的转义 \\{}", other as char)),
                    }
                }
                _ => {
                    // 原样字符：一直切到下一个 `"` / `\`，整段搬过去。两者都是 ASCII，不会落在多字节字符中间，
                    // 所以切出来的一定是合法的 `&str`。**不能**每个字符都对剩下的整段做一次 `from_utf8`：
                    // 那是 O(n²)，贴吧关注缓冲在手机上一次解析能卡好几分钟。
                    let start = self.i;
                    let end = self.bytes[start..]
                        .iter()
                        .position(|&b| b == b'"' || b == b'\\')
                        .map_or(self.bytes.len(), |n| start + n);
                    out.push_str(&self.text[start..end]);
                    self.i = end;
                }
            }
        }
    }

    fn hex4(&mut self) -> Result<u32, String> {
        let hex = self
            .bytes
            .get(self.i..self.i + 4)
            .and_then(|b| std::str::from_utf8(b).ok())
            .ok_or_else(|| "\\u 后面不足 4 位".to_string())?;
        let v = u32::from_str_radix(hex, 16).map_err(|_| format!("\\u{hex} 不是十六进制"))?;
        self.i += 4;
        Ok(v)
    }

    /// 数字：保留原始字面量（`-1.5e10` 整段照抄）。
    fn number(&mut self) -> Result<J, String> {
        let start = self.i;
        if self.peek() == Some(b'-') {
            self.i += 1;
        }
        while let Some(c) = self.peek() {
            let keep = c.is_ascii_digit() || matches!(c, b'.' | b'e' | b'E' | b'+' | b'-');
            if keep {
                self.i += 1;
            } else {
                break;
            }
        }
        if start == self.i {
            return Err(format!("第 {} 字节不是数字", start));
        }
        let raw = std::str::from_utf8(&self.bytes[start..self.i])
            .map_err(|_| "数字不是合法 UTF-8".to_string())?
            .to_string();
        Ok(J::Num(raw))
    }
}

/// 把一个字符串写成 JSON 字符串字面量（**kotlinx.serialization 的口径**，和
/// [`crate::hosted::push_json_string`] 逐字节相同：`"` `\` `\n` `\r` `\t` `\b` `\f` 用短转义，
/// 其余 `< 0x20` 的用 `\u00xx`（**小写**），别的字符（含 DEL、U+2028、emoji）原样写）。
pub fn quote(out: &mut String, s: &str) {
    crate::hosted::push_json_string(out, s);
}

