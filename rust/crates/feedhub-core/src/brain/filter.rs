//! 内容过滤：平台标的 18+、屏蔽词、外出模式（关键词 / 社区）。
//!
//! 一一对应 Kotlin 的 `core/AdultFilter.kt` + `core/ContentFilter.kt`（第一层）。**一条都不能变**：
//!
//! - **命中就整条隐藏，不打码、不折叠**；
//! - **屏蔽词**一直生效；**外出模式**的关键词 / 社区只在外出模式开着时生效（关掉之后"我们自己判出来的"
//!   那部分照常显示）；平台标的 18+ **一直隐藏**，和外出模式无关；
//! - 匹配口径：**不分大小写、全半角当同一个字**（`Ａ` = `A`、`：` = `:`、全角空格 = 空格），
//!   连续空白压成一个；关键词是**子串**匹配，**社区是全名相等**（不做子串，免得"原神"把"原神内鬼吧"也藏了）；
//! - 看的文字：标题、摘要、转发那条自己的标题 / 摘要；社区看 `community`（转发的原内容自己的社区也算）；
//! - **繁体 / 简体先不做**。
//!
//! `normalize` 的口径和 Kotlin 逐字符对齐：Kotlin 的 `for (raw in text)` 走的是 **UTF-16 码元**，
//! `lowercaseChar()` 是 Java 的**简单小写映射**（一个码元进、一个码元出）。Rust 这边按字符走，
//! 有三处要照 Kotlin 的口径写死（不然带某些字符时两边的"藏不藏"会不一样）：
//!
//! 1. `Char.lowercaseChar()` 对**补充平面**的字符返回原样（代理对是两个码元，Java 的
//!    `Character.toLowerCase(char)` 不改孤立代理项）——所以 `U+10000` 以上**不小写**；
//! 2. `U+0130`（İ）这类"全小写映射成多个字符"的，Java 的简单映射取**第一个**（`i`），
//!    Rust 的 `char::to_lowercase()` 会给 `i` + 组合点两个字符，这里只取第一个；
//! 3. 空白只认 `' '` / `\t` / `\n` / `\r`（外加被折成空格的 `U+3000` / `U+00A0`），
//!    **不是** Rust 的 `char::is_whitespace`（它多认 U+0085、少了 U+001C..U+001F 的差别）。

/// 一条内容里过滤要看的那几个字段（别的字段过滤不看，不搬）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MixText {
    /// 平台标的 18+（`FeedItem.adult`）。和内容名单无关，一直隐藏。
    pub adult: bool,
    pub title: Option<String>,
    pub summary: Option<String>,
    /// 社区（贴吧吧名、Reddit `r/…`、知乎问题标题、酷安话题、抖音话题……）。
    pub community: Option<String>,
    /// 转发那条自己的标题 / 摘要 / 社区（`FeedItem.repost`）。
    pub repost_title: Option<String>,
    pub repost_summary: Option<String>,
    pub repost_community: Option<String>,
}

/// 这次过滤用的规则（和 `AppSettings` 的那四个一一对应，每一批现读）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Rules {
    /// 屏蔽词，一直生效。
    pub block_words: Vec<String>,
    /// 外出模式开着没有。
    pub outing_mode: bool,
    /// 外出模式的关键词，只在外出模式开着时生效。
    pub outing_keywords: Vec<String>,
    /// 外出模式的社区（全名相等），只在外出模式开着时生效。
    pub outing_communities: Vec<String>,
}

impl Rules {
    /// 现在这条规则会不会藏东西。空名单 = 什么都不用做（调用方据此走"原样返回"的快路径；
    /// 外出模式关着时，它那两份名单再长也不算数）。
    pub fn is_empty(&self) -> bool {
        self.block_words.is_empty()
            && (!self.outing_mode
                || (self.outing_keywords.is_empty() && self.outing_communities.is_empty()))
    }
}

/// 为什么被藏掉（只给日志 / 调试用；界面上一律"整条隐藏"，不告诉用户是哪条规则）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    BlockWord,
    OutingKeyword,
    OutingCommunity,
}

/// 命中的那条规则（`rule` 是用户写的原文，方便在调试日志里对是哪一条）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    pub reason: Reason,
    pub rule: String,
}

/// 一批过完的结果：保留下来的下标（输入顺序）+ 藏掉几条。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Visible {
    pub kept: Vec<u32>,
    pub hidden: u32,
}

/// 这条要不要整条藏掉：命中就返回是哪条规则，没有就是 `None`。
///
/// 顺序：屏蔽词（一直生效）→ 外出模式关键词 → 外出模式社区；**外出模式关着时后两项完全不看**。
///
/// **不看 `adult`**（和 Kotlin `ContentFilter.hidden` 一样）：平台标成 18+ 的一直由 18+ 那一条藏，
/// 和这里无关。要"18+ 或名单"的合判用 [`blocked`] / [`visible`]。
pub fn hidden(text: &MixText, rules: &Rules) -> Option<Hit> {
    if rules.is_empty() {
        return None;
    }
    let texts: Vec<String> = texts_of(text).iter().map(|t| normalize(t)).collect();
    for word in &rules.block_words {
        if matches_text(&texts, word) {
            return Some(Hit {
                reason: Reason::BlockWord,
                rule: word.clone(),
            });
        }
    }
    if !rules.outing_mode {
        return None;
    }
    for word in &rules.outing_keywords {
        if matches_text(&texts, word) {
            return Some(Hit {
                reason: Reason::OutingKeyword,
                rule: word.clone(),
            });
        }
    }
    for community in &rules.outing_communities {
        if matches_community(text, community) {
            return Some(Hit {
                reason: Reason::OutingCommunity,
                rule: community.clone(),
            });
        }
    }
    None
}

/// 一条内容**为什么**不能显示：18+ 或者命中名单。
///
/// 这个和 [`hidden`] 分开，是为了让"18+ 一直生效"这条规则只有一个地方实现（[`visible`] 用它）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Blocked {
    /// 平台标的 18+（`Reason` 里没有这一档：它不是名单规则，界面 / 日志都按"平台标的"说）。
    Adult,
    /// 命中一条名单规则。
    Rule(Hit),
}

/// 单条判断：18+ 或命中名单就返回原因。
pub fn blocked(text: &MixText, rules: &Rules) -> Option<Blocked> {
    if text.adult {
        return Some(Blocked::Adult);
    }
    hidden(text, rules).map(Blocked::Rule)
}

/// 一批里**按名单规则**能显示的那些（下标，输入顺序）+ 藏掉几条（`ContentFilter.visible`）。
///
/// **不看 18+**（和 `ContentFilter.hidden` 一样）；名单空时原样全留。
pub fn visible(items: &[MixText], rules: &Rules) -> Visible {
    if items.is_empty() {
        return Visible {
            kept: Vec::new(),
            hidden: 0,
        };
    }
    let mut kept = Vec::with_capacity(items.len());
    let mut hidden_count = 0u32;
    for (i, item) in items.iter().enumerate() {
        if hidden(item, rules).is_some() {
            hidden_count += 1;
        } else {
            kept.push(i as u32);
        }
    }
    Visible {
        kept,
        hidden: hidden_count,
    }
}

/// 一批里**能上屏**的那些：18+（一直生效）或者命中名单都藏掉（信息流 / 搜索的 `kept`）。
pub fn shown(items: &[MixText], rules: &Rules) -> Visible {
    if items.is_empty() {
        return Visible {
            kept: Vec::new(),
            hidden: 0,
        };
    }
    let mut kept = Vec::with_capacity(items.len());
    let mut hidden_count = 0u32;
    for (i, item) in items.iter().enumerate() {
        if blocked(item, rules).is_some() {
            hidden_count += 1;
        } else {
            kept.push(i as u32);
        }
    }
    Visible {
        kept,
        hidden: hidden_count,
    }
}

/// 一条内容里过滤看的文字（标题、摘要、转发那条自己的标题 / 摘要），顺序照 Kotlin 的 `textOf`。
fn texts_of(text: &MixText) -> Vec<&str> {
    let mut out = Vec::with_capacity(4);
    out.extend(text.title.as_deref());
    out.extend(text.summary.as_deref());
    out.extend(text.repost_title.as_deref());
    out.extend(text.repost_summary.as_deref());
    out
}

/// 关键词命中：`texts` 是规整之后的标题 / 摘要 / 转发原文。
fn matches_text(texts: &[String], word: &str) -> bool {
    let needle = normalize(word);
    if needle.is_empty() {
        return false;
    }
    texts.iter().any(|text| text.contains(&needle))
}

/// 社区命中：全名相等（`community` 或转发的原内容自己的社区）。
fn matches_community(text: &MixText, community: &str) -> bool {
    let needle = normalize(community);
    if needle.is_empty() {
        return false;
    }
    if let Some(value) = text.community.as_deref() {
        if normalize(value) == needle {
            return true;
        }
    }
    match text.repost_community.as_deref() {
        Some(value) => normalize(value) == needle,
        None => false,
    }
}

/// 把一条关键词 / 社区名 / 文本归一成用来比较的样子：去首尾空白、全角转半角、小写、连续空白压成一个。
///
/// 和 Kotlin `ContentFilter.normalize` 逐字符对齐（见模块说明的三条口径）。
pub fn normalize(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut space = false;
    for raw in text.chars() {
        // U+3000（全角空格）/ U+00A0（不换行空格）都当普通空格
        let c = if raw == '\u{3000}' || raw == '\u{00A0}' {
            ' '
        } else if ('\u{FF01}'..='\u{FF5E}').contains(&raw) {
            // 全角 ASCII → 半角
            char::from_u32(raw as u32 - 0xFEE0).unwrap_or(raw)
        } else {
            raw
        };
        if c == ' ' || c == '\t' || c == '\n' || c == '\r' {
            space = true;
            continue;
        }
        if space && !out.is_empty() {
            out.push(' ');
        }
        space = false;
        out.push(simple_lowercase(c));
    }
    out
}

/// Kotlin `Char.lowercaseChar()`（Java `Character.toLowerCase(char)`）的口径：
/// 一个字符进、一个字符出；补充平面的字符**原样返回**（Kotlin 走 UTF-16 码元，代理项不会被改）。
fn simple_lowercase(c: char) -> char {
    if (c as u32) > 0xFFFF {
        return c;
    }
    let mut iter = c.to_lowercase();
    match (iter.next(), iter.next()) {
        // 全小写映射成多个字符的（现在只有 U+0130）：Java 的简单映射取第一个
        (Some(first), _) => first,
        (None, _) => c,
    }
}

// ===========================================================================
// 名单的归拢
// ===========================================================================

/// 一份名单最多几条（和设置页的输入限制同一个数；读存档时也按它收）。
pub const MAX_RULES: usize = 200;
/// 一条最多几个字。**按 Kotlin `String.length` 算**（UTF-16 码元），不是 Unicode 码点。
pub const MAX_RULE_LENGTH: usize = 40;

/// 读存档 / 设置页加一条时把名单收拾干净：去空白、丢掉空的和过长的、按 [`normalize`] 出来的样子去重、
/// 最多 [`MAX_RULES`] 条。**保留用户写的原文**（不把 `Ａ` 改成 `A`），只是比较时忽略这些差别。
///
/// 和 Kotlin `ContentFilter.normalizeRules` 逐字节一致，三处容易漂的地方：
/// 1. `trim()` 用 [`crate::hosted::kotlin_trim`] 的**真口径**（Kotlin 的 `Char.isWhitespace()` 认 U+00A0 /
///    U+2007 / U+202F 这三个 `Character.isSpaceChar` 才有的字符，Rust 的 `str::trim` 口径不同）；
/// 2. 长度按 **UTF-16 码元**数（Kotlin `String.length`）：一个 emoji 算 2；
/// 3. 去重键是 [`normalize`] 之后的样子，写回去的是**原文**（`Cat` 和 `cat` 算同一条，留下先出现的那个）。
pub fn normalize_rules(words: &[String]) -> Vec<String> {
    let mut seen: Vec<String> = Vec::with_capacity(words.len());
    let mut out: Vec<String> = Vec::with_capacity(words.len());
    for raw in words {
        let word = crate::hosted::kotlin_trim_blank(raw);
        if word.is_empty() || utf16_len(word) > MAX_RULE_LENGTH {
            continue;
        }
        let key = normalize(word);
        if seen.iter().any(|s| s == &key) {
            continue;
        }
        seen.push(key);
        out.push(word.to_string());
        if out.len() >= MAX_RULES {
            break;
        }
    }
    out
}

/// Kotlin `String.length`：UTF-16 码元个数（补充平面的字符算 2）。
pub fn utf16_len(text: &str) -> usize {
    text.encode_utf16().count()
}
