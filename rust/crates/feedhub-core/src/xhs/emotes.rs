//! 表情表（Kotlin `XhsEmoteTable` + `XhsSource` 表情那一段的 Rust 版）。
//!
//! 表是什么：文字代码（`[笑哭R]`、`[doge]`）→ 图片地址（`picasso-static.xiaohongshu.com/fe-platform/<hash>.png`，
//! 静态资源、无签名）。来自 `GET /api/im/redmoji/detail`：`data.emoji.tabs[].collection[].emoji[]` =
//! `{image_name, image}`。
//! `/api/im/redmoji/version` 是版本号（当时是 5），我们不用它。
//!
//! 现在表住在这个模块 + 平台对象里：
//! 详情 / 评论 / 楼中楼直接用 Rust 里那份，**不再每次从 Kotlin 传**；只有信息流 / 搜索 / 关注流
//! 还过一次边界（那时传的是"这段文字里真出现过的那几项"，不是整张表，见
//! [`crate::xhs::source::XhsHostedSource::emotes_for`]）。
//!
//! 三条要保住的口径（和 Kotlin 逐条对齐，见各函数）：
//! 1. **缓存一周**（[`TTL_SEC`]），存 `ctx.store` 的 [`KEY`]，格式是 `{"v":1,"at":<秒>,"map":{…}}`
//!    ——**老数据照读**（Kotlin 写的那些）；
//! 2. 同一时间**只许一个在飞的请求**（Kotlin `emoteFetching` 的 `AtomicBoolean`）；
//! 3. 取不到就**降级成文字**：`SourceException` / `IOException` 之后 10 分钟不再试，用旧的那份。

use crate::json;
use crate::json::JsonMap;
use crate::xhs::codec;

/// `ctx.store` 里的键（和 Kotlin `XhsEmoteTable.KEY` 一样，老数据照读）。
pub const KEY: &str = "xhs.emotes";

/// 取表的路径（`GET`，**不带** `x-rap-param` / `xy-direction`：`XhsApi.RAP_PATHS` 里没有它）。
pub const PATH: &str = "/api/im/redmoji/detail";

/// 缓存一周（和 Kotlin `XhsEmoteTable.TTL_SEC` 一样）。
pub const TTL_SEC: i64 = 7 * 24 * 3600;

/// 失败之后多少毫秒内不再试（Kotlin `6 * 60 * 60 * 1000` 是 6 小时？不是——Kotlin 是 10 分钟，
/// 见 `XhsSource.emoteTable` 的 `emoteRetryAtMs = nowMs + 10 * 60_000L`）。
pub const RETRY_MS: i64 = 10 * 60_000;

/// 存储格式的版本（Kotlin `XhsEmoteTable.VERSION`）。
const VERSION: i64 = 1;

/// 表：文字代码 → 图片地址（**保留插入顺序**：Kotlin 那边是 `LinkedHashMap`，
/// `Emotes.used` 挑出来的顺序就是表的顺序）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EmoteTable {
    pub map: Vec<(String, String)>,
    /// 取到这份表的时间（Unix 秒）。
    pub fetched_at_sec: i64,
}

impl EmoteTable {
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// `XhsEmoteTable.toStored`：`{"v":1,"at":<秒>,"map":{…}}`。
    ///
    /// 字段顺序照 kotlinx.serialization 那个 `@Serializable class Stored(v, at, map)`；
    /// 字符串转义用 [`codec::json_string`]（换行 / 引号 / 控制字符，表情代码和地址里都没有）。
    pub fn to_stored(&self) -> String {
        let mut out = String::with_capacity(64 + self.map.len() * 64);
        out.push_str("{\"v\":");
        out.push_str(&VERSION.to_string());
        out.push_str(",\"at\":");
        out.push_str(&self.fetched_at_sec.to_string());
        out.push_str(",\"map\":{");
        for (i, (code, url)) in self.map.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            out.push_str(&codec::json_string(code));
            out.push(':');
            out.push_str(&codec::json_string(url));
        }
        out.push_str("}}");
        out
    }

    /// [`Self::to_stored`] 的逆运算。**读不出来就是 `None`**（版本不对 / JSON 坏了 / 没有 map），
    /// 调用方当成"没有表"重新取（Kotlin 也是 `runCatching{}.getOrNull()`）。
    pub fn from_stored(text: &str) -> Option<EmoteTable> {
        let root = json::parse_object(text).ok()?;
        if json::long(&root, "v") != Some(VERSION) {
            return None;
        }
        Some(EmoteTable {
            // `map` 的顺序**必须保持文本里的顺序**（`Emotes.used` 挑出来的顺序就是表的顺序），
            // 所以不用 `serde_json` 的 Map（那是按键名排序的）：自己扫一遍字符串对。
            map: read_map_in_order(text).unwrap_or_default(),
            fetched_at_sec: json::long(&root, "at").unwrap_or(0),
        })
    }

    /// `XhsEmoteTable.fromDetail`：`data.emoji.tabs[].collection[].emoji[]` 里的 `image_name` + `image`。
    ///
    /// 同一个代码出现两次时**先出现的为准**（Kotlin `putIfAbsent`）；缺 `image_name` / `image` 的项跳过；
    /// `image` 统一成 https（`toHttps`）。`now_sec` 就是这份表的"取到时间"。
    pub fn from_detail(root: &JsonMap, now_sec: i64) -> EmoteTable {
        let mut map: Vec<(String, String)> = Vec::new();
        let tabs = json::obj(root, "data")
            .and_then(|d| json::obj(d, "emoji"))
            .and_then(|e| json::arr(e, "tabs"));
        for tab in tabs.map(|v| v.as_slice()).unwrap_or(&[]) {
            let Some(tab) = tab.as_object() else { continue };
            for collection in json::arr(tab, "collection").map(|v| v.as_slice()).unwrap_or(&[]) {
                let Some(collection) = collection.as_object() else { continue };
                for emoji in json::arr(collection, "emoji").map(|v| v.as_slice()).unwrap_or(&[]) {
                    let Some(emoji) = emoji.as_object() else { continue };
                    let Some(name) = json::text(emoji, "image_name") else { continue };
                    let Some(image) = json::text(emoji, "image") else { continue };
                    if map.iter().any(|(k, _)| *k == name) {
                        continue;
                    }
                    map.push((name, json::to_https(&image)));
                }
            }
        }
        EmoteTable {
            map,
            fetched_at_sec: now_sec,
        }
    }

    /// 从表里挑出 `texts` 里真的出现过的代码（`Emotes.used`：保持表的顺序，空的跳过）。
    pub fn used(&self, texts: &[Option<&str>]) -> Vec<(String, String)> {
        crate::xhs::emotes_used(&self.map, texts)
    }
}

/// 从 JSON 文本里读 `"map"` 那个对象，**保持键在文本里的顺序**。
///
/// 为什么不用 `serde_json`：它默认的 `Map` 是 `BTreeMap`（按键名排序），而表情表的顺序就是
/// `Emotes.used` 挑出来的顺序。
/// 表的值都是字符串（`{"[代码]":"地址"}`），所以这里只认字符串对、够用。
fn read_map_in_order(text: &str) -> Option<Vec<(String, String)>> {
    let bytes = text.as_bytes();
    let start = find_map_object(bytes)?;
    let mut i = start;
    let mut out = Vec::new();
    skip_ws(bytes, &mut i);
    if *bytes.get(i)? != b'{' {
        return None;
    }
    i += 1;
    loop {
        skip_ws(bytes, &mut i);
        match *bytes.get(i)? {
            b'}' => return Some(out),
            b',' => {
                i += 1;
                continue;
            }
            b'"' => {}
            _ => return None,
        }
        let (key, next) = read_json_string(bytes, i)?;
        i = next;
        skip_ws(bytes, &mut i);
        if *bytes.get(i)? != b':' {
            return None;
        }
        i += 1;
        skip_ws(bytes, &mut i);
        let (value, next) = read_json_string(bytes, i)?;
        i = next;
        out.push((key, value));
    }
}

/// `"map"` 后面的那个 `{` 的下标。只认"顶层键"那种位置（前面是 `{` 或 `,`），
/// 免得把某个字符串值里的 `"map"` 当成键。
fn find_map_object(bytes: &[u8]) -> Option<usize> {
    let needle = b"\"map\"";
    let mut i = 0;
    while i + needle.len() <= bytes.len() {
        if &bytes[i..i + needle.len()] == needle {
            let mut before = i;
            while before > 0 && (bytes[before - 1] as char).is_ascii_whitespace() {
                before -= 1;
            }
            let key_position = before == 0 || bytes[before - 1] == b'{' || bytes[before - 1] == b',';
            let mut after = i + needle.len();
            skip_ws(bytes, &mut after);
            if key_position && bytes.get(after) == Some(&b':') {
                after += 1;
                skip_ws(bytes, &mut after);
                if bytes.get(after) == Some(&b'{') {
                    return Some(after);
                }
            }
        }
        i += 1;
    }
    None
}

fn skip_ws(bytes: &[u8], i: &mut usize) {
    while *i < bytes.len() && (bytes[*i] as char).is_ascii_whitespace() {
        *i += 1;
    }
}

/// 读一个 JSON 字符串（`i` 指着开头的 `"`），返回（解码后的值，指向结束引号之后的下标）。
fn read_json_string(bytes: &[u8], mut i: usize) -> Option<(String, usize)> {
    if bytes.get(i) != Some(&b'"') {
        return None;
    }
    i += 1;
    let mut out = String::new();
    loop {
        let c = *bytes.get(i)?;
        match c {
            b'"' => return Some((out, i + 1)),
            b'\\' => {
                i += 1;
                let esc = *bytes.get(i)?;
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
                        let hi = read_hex4(bytes, i + 1)?;
                        i += 4;
                        // 代理对：高位后面跟 `\uDC00`–`\uDFFF` 时合成一个字符
                        if (0xD800..0xDC00).contains(&hi)
                            && bytes.get(i + 1) == Some(&b'\\')
                            && bytes.get(i + 2) == Some(&b'u')
                        {
                            if let Some(lo) = read_hex4(bytes, i + 3).filter(|l| (0xDC00..0xE000).contains(l)) {
                                i += 6;
                                let code = 0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00);
                                out.push(char::from_u32(code)?);
                            } else {
                                out.push(char::from_u32(hi)?);
                            }
                        } else {
                            out.push(char::from_u32(hi)?);
                        }
                    }
                    _ => return None,
                }
                i += 1;
            }
            _ => {
                // 原样的字节：按 UTF-8 解码（表里都是 UTF-8 原文）
                let rest = std::str::from_utf8(&bytes[i..]).ok()?;
                let ch = rest.chars().next()?;
                out.push(ch);
                i += ch.len_utf8();
            }
        }
    }
}

fn read_hex4(bytes: &[u8], at: usize) -> Option<u32> {
    let hex = std::str::from_utf8(bytes.get(at..at + 4)?).ok()?;
    u32::from_str_radix(hex, 16).ok()
}

