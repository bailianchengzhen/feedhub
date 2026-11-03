//! 抖音的表情表（Kotlin `DouyinEmoteTable` + `DouyinSource.emotesFor` 的 Rust 版）。
//!
//! 表是什么：文字代码（`[赞]` / `[笑哭]`）→ 图片地址。来自 `GET /aweme/v1/web/emoji/list`
//! （`channel=channel_pc_web`）；**离线资料里没有响应结构的出处**，所以解析照 Kotlin 用"按常见键名找代码、
//! 递归找第一个 http 地址"的宽松取法，认不出就是空表（不编）。
//!
//! **为什么搬到 Rust**：以前表归 Kotlin、Rust 只按表切 Span，所以**每一次**
//! 详情 / 评论 / 推荐流都要先把 `desc` / 正文拼串交回 Kotlin、Kotlin 判断要不要取表、再拿表把整个 body
//! 映射第二遍（多过一次 FFI、多一次整份 body 的解析）。现在表住在这个模块 + 平台对象里：
//! 详情 / 评论 / 楼中楼 / 推荐流在 Rust 内部直接用，**只有"这一段文字里真出现过的那几项"还需要过一次边界**
//! （`FeedItem.emotes` / `Span.Emote` 里本来就是那几个键值对）。
//!
//! 三条要保住的口径（和 Kotlin 逐条对齐，见各函数）：
//! 1. **缓存一周**（[`TTL_SEC`]），存 `ctx.store` 的 [`KEY`]，格式是 `{"v":1,"at":<秒>,"map":{…}}`
//!    ——**老数据照读**（Kotlin 写的那些，`kotlinx.serialization` 的 `Stored(v, at, map)`，`map` 是
//!    `LinkedHashMap` 的顺序）；
//! 2. 失败（不是风控）之后 [`RETRY_MS`] 内不再试，先用旧的表（可能空）；**风控照常往外抛**，不动重试时间；
//! 3. 一张表**同一时间只许一个在飞**的请求（Kotlin 没有做这一步，但和 `xhs/emotes.rs` 对齐：
//!    表是锦上添花，不该在首页并发刷新时重复占风控名额）。
//!
//! 存储格式的读写逐字节照 Kotlin（[`EmoteTable::to_stored`] / [`EmoteTable::from_stored`]）：
//! `map` 的**顺序**要保住（`Emotes.used` 挑出来的顺序就是表的顺序，`FeedItem.emotes` 也按它），
//! 所以这里不用 `serde_json` 的 Map（那是按键名排序的）。

use crate::json;
use crate::json::JsonMap;
use crate::xhs::jsonord::{self, J};

/// `ctx.store` 里的键（和 Kotlin `DouyinEmoteTable.KEY` 一样，老数据照读）。
pub const KEY: &str = "douyin.emotes";

/// 取表的路径（`GET`，带 `channel=channel_pc_web`，**不带**业务码判定：`status_code != 0` 就是空表）。
pub const PATH: &str = "/aweme/v1/web/emoji/list";

/// 缓存一周（和 Kotlin `DouyinEmoteTable.TTL_SEC` 一样）。
pub const TTL_SEC: i64 = 7 * 24 * 3600;

/// 失败之后多少毫秒内不再试（Kotlin `DouyinSource.EMOTE_RETRY_MS` = 10 分钟）。
pub const RETRY_MS: i64 = 10 * 60_000;

/// 存储格式的版本（Kotlin `DouyinEmoteTable.VERSION`）。
const VERSION: i64 = 1;

/// 表：文字代码 → 图片地址（**保留插入顺序**：Kotlin 那边是 `LinkedHashMap`）。
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

    /// `DouyinEmoteTable.toStored`：`{"v":1,"at":<秒>,"map":{…}}`。
    ///
    /// 字段顺序照 kotlinx.serialization 那个 `@Serializable class Stored(v, at, map)`；
    /// 字符串转义用 [`jsonord::quote`]（kotlinx 的口径：`\u00xx` **小写**，DEL / U+2028 / emoji 原样）。
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
            jsonord::quote(&mut out, code);
            out.push(':');
            jsonord::quote(&mut out, url);
        }
        out.push_str("}}");
        out
    }

    /// [`Self::to_stored`] 的逆运算。**读不出来就是 `None`**（版本不对 / JSON 坏了），
    /// 调用方当成"没有表"重新取（Kotlin 也是 `runCatching{}.getOrNull()`）。
    ///
    /// `map` 的顺序保持文本里的顺序（[`read_map_in_order`]），所以"Rust 读进来再写出去"和 Kotlin 逐字节一样。
    pub fn from_stored(text: &str) -> Option<EmoteTable> {
        let root: J = jsonord::parse(text).ok()?;
        if root.get("v")?.as_i64() != Some(VERSION) {
            return None;
        }
        // `at` / `map` 都是 kotlinx 那边的**必填字段**：缺一个整份解码就失败（`runCatching{}.getOrNull()`
        // → 当成没有表）。这里照抄，不给默认值。
        let at = root.get("at")?.as_i64()?;
        let map = read_map_in_order(&root)?;
        Some(EmoteTable {
            map,
            fetched_at_sec: at,
        })
    }

    /// `DouyinEmoteTable.fromList`：`emoji_list` / `data.list` / `stickers` 那几种位置里的
    /// `{代码, 图片}`；认不出来就返回空表（**不报错**）。
    ///
    /// Kotlin 的 `candidateLists` 是"按名单先试、都不行递归找前 3 个对象数组"；同一批名单里
    /// 先出现的代码为准（`putIfAbsent`），图片地址统一成 https（`toHttps`）。`now_sec` 是"取到的时间"。
    pub fn from_list(root: &JsonMap, now_sec: i64) -> EmoteTable {
        let mut map: Vec<(String, String)> = Vec::new();
        for list in candidate_lists(root) {
            for entry in list {
                let Some(o) = entry.as_object() else { continue };
                let Some(code) = code_of(o) else { continue };
                let Some(url) = image_of(o) else { continue };
                if map.iter().any(|(k, _)| *k == code) {
                    continue;
                }
                map.push((code, json::to_https(&url)));
            }
            if !map.is_empty() {
                break;
            }
        }
        EmoteTable {
            map,
            fetched_at_sec: now_sec,
        }
    }

    /// 从表里挑出 `texts` 里真的出现过的代码（`Emotes.used`：保持表的顺序，空的跳过）。
    pub fn used(&self, texts: &[Option<&str>]) -> Vec<(String, String)> {
        // `Emotes.used`：按表的顺序挑出真的出现在 `texts` 里的代码（空的跳过）
        if self.map.is_empty() {
            return Vec::new();
        }
        let present: Vec<&str> = texts.iter().flatten().copied().filter(|t| !t.is_empty()).collect();
        if present.is_empty() {
            return Vec::new();
        }
        self.map
            .iter()
            .filter(|(code, url)| {
                !code.is_empty() && !url.is_empty() && present.iter().any(|t| t.contains(code.as_str()))
            })
            .cloned()
            .collect()
    }
}

/// 文本里有没有 `[xxx]` 这样的表情记号（Kotlin `DouyinEmoteTable.EMOTE_TOKEN`
/// = `\[[^\[\]"'<>]{1,16}]`）。决定"这一趟要不要去取表情表"。
///
/// 自己扫，不引 regex crate（和 `xhs/detail.rs` 的 `has_emote_token` 一个路子）：
/// `{1,16}` 按**字符**数（Kotlin 按 UTF-16 码元，只有名字里有 emoji 这类星平面字符时两者才会差 1，
/// 那种情况下多算一个字符只可能让"要不要取表"多判一次 true，不影响挑出来的项）。
pub fn has_emote_token(text: &str) -> bool {
    let mut i = 0;
    while i < text.len() {
        let c = text[i..].chars().next().unwrap_or('\u{fffd}');
        if c == '[' && emote_token_end(text, i).is_some() {
            return true;
        }
        i += c.len_utf8();
    }
    false
}

/// 一棵 JSON 里有没有哪个**字符串值**带表情记号（Kotlin `json.anyString { EMOTE_TOKEN.containsMatchIn(it) }`）。
///
/// 只看字符串值、不看键：表情代码出现在正文里。边走树边扫，不把整棵树重新序列化。
pub fn tree_has_emote_token(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::String(s) => has_emote_token(s),
        serde_json::Value::Array(list) => list.iter().any(tree_has_emote_token),
        serde_json::Value::Object(map) => map.values().any(tree_has_emote_token),
        _ => false,
    }
}

/// `text[from]` 是 `[` 时，这里有没有一个合法的表情记号；有就返回它的结束下标（`]` 之后）。
///
/// 口径：`[` 之后 1～16 个字符，字符里**不能有** `[` `]` `"` `'` `<` `>`，紧接着必须是 `]`。
fn emote_token_end(text: &str, from: usize) -> Option<usize> {
    if !text[from..].starts_with('[') {
        return None;
    }
    let mut i = from + 1;
    let mut count = 0usize;
    while i < text.len() {
        let c = text[i..].chars().next()?;
        if c == ']' {
            return if count >= 1 { Some(i + 1) } else { None };
        }
        if matches!(c, '[' | '"' | '\'' | '<' | '>') {
            return None;
        }
        count += 1;
        if count > 16 {
            return None;
        }
        i += c.len_utf8();
    }
    None
}

// ===========================================================================
// from_list 的内部：找列表 / 找代码 / 找图片（逐条对照 Kotlin 的 companion 函数）
// ===========================================================================

/// Kotlin `candidateLists`：先按常见位置试（顶层 `emoji_list`、`data.emoji_list`、`data.list`、
/// `stickers`、`data.stickers`、`list`），一个都没有就递归找**前 3 个**对象数组。
fn candidate_lists(root: &JsonMap) -> Vec<&Vec<serde_json::Value>> {
    let data = json::obj(root, "data");
    let named = [
        json::arr(root, "emoji_list"),
        data.and_then(|d| json::arr(d, "emoji_list")),
        data.and_then(|d| json::arr(d, "list")),
        json::arr(root, "stickers"),
        data.and_then(|d| json::arr(d, "stickers")),
        json::arr(root, "list"),
    ];
    let found: Vec<&Vec<serde_json::Value>> = named.into_iter().flatten().collect();
    if !found.is_empty() {
        return found;
    }
    let mut deep = Vec::new();
    deep_arrays(root, &mut deep);
    deep.truncate(3);
    deep
}

/// Kotlin `deepArrays`：按文档顺序递归收集"至少有一个对象元素"的数组。
fn deep_arrays<'a>(o: &'a JsonMap, out: &mut Vec<&'a Vec<serde_json::Value>>) {
    for v in o.values() {
        match v {
            serde_json::Value::Array(list) => {
                if list.iter().any(|e| e.is_object()) {
                    out.push(list);
                }
            }
            serde_json::Value::Object(inner) => deep_arrays(inner, out),
            _ => {}
        }
    }
}

/// Kotlin `CODE_KEYS` 的顺序（先出现的为准）。
const CODE_KEYS: [&str; 8] = [
    "display_name",
    "emoji_name",
    "name",
    "text",
    "code",
    "display_name_zh",
    "desc",
    "label",
];

/// 这一项的文字代码：上面那串键里第一个"`[` 开头、`]` 结尾、长度 ≥ 3"的值。
fn code_of(o: &JsonMap) -> Option<String> {
    for key in CODE_KEYS {
        let Some(v) = json::string(o, key) else { continue };
        if v.chars().count() >= 3 && v.starts_with('[') && v.ends_with(']') {
            return Some(v);
        }
    }
    None
}

/// Kotlin `URL_KEYS` 的顺序。
const URL_KEYS: [&str; 8] = [
    "emoji_url",
    "url_list",
    "icon",
    "image",
    "static_url",
    "url",
    "src",
    "uri",
];

/// 这一项的图片地址：先按常见键名找（`url_list` 取第一项、对象就递归找第一个 http 地址），
/// 都找不到就**递归找第一个 http 开头的字符串**（Kotlin `stringIn`）。
fn image_of(o: &JsonMap) -> Option<String> {
    for key in URL_KEYS {
        let Some(v) = o.get(key) else { continue };
        if let Some(found) = http_in_value(v) {
            return Some(found);
        }
    }
    http_in_object(o)
}

/// 一个值里的第一个 http 地址（`JsonPrimitive` 直接判、数组取第一项、对象递归）。
fn http_in_value(v: &serde_json::Value) -> Option<String> {
    match v {
        serde_json::Value::String(s) => http_prefix(s),
        serde_json::Value::Number(n) => http_prefix(&n.to_string()),
        serde_json::Value::Array(list) => list.iter().find_map(http_in_value),
        serde_json::Value::Object(inner) => http_in_object(inner),
        _ => None,
    }
}

/// 一个对象（按文档顺序）里第一个 http 地址。
fn http_in_object(o: &JsonMap) -> Option<String> {
    o.values().find_map(http_in_value)
}

fn http_prefix(s: &str) -> Option<String> {
    if s.starts_with("http") {
        Some(s.to_string())
    } else {
        None
    }
}

/// 从存盘文本里读 `"map"` 那个对象，**保持键在文本里的顺序**。
///
/// 为什么不用 `serde_json`：它默认的 `Map` 是 `BTreeMap`（按键名排序），而表情表的顺序就是
/// `Emotes.used` 挑出来的顺序。表的值都是字符串（`{"[代码]":"地址"}`），所以这里只认字符串对、够用。
fn read_map_in_order(root: &J) -> Option<Vec<(String, String)>> {
    let map = root.get("map")?;
    match map {
        J::Obj(fields) => {
            let mut out = Vec::with_capacity(fields.len());
            for (k, v) in fields {
                out.push((k.clone(), v.as_str()?.to_string()));
            }
            Some(out)
        }
        _ => None,
    }
}

