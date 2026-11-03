//! **HTML → 树 / 富文本（全仓唯一一份）**。
//!
//! 两份都"按 jsoup 口径"，但各修过不同的坑，会慢慢漂开——所以合成一份：
//!
//! - [`dom`]：**HTML5 分词器 + "in body" 树构造的最小 DOM**（jsoup 1.18.3 的替身，
//!   加固版：`MAX_DEPTH`、迭代 drop / 迭代遍历、256 KB 小栈硬门）。**所有 HTML 正文都走它**：
//!   知乎（[`crate::zhihu::html`]）、小黑盒（[`crate::heybox::detail`] 的 `html_blocks`）。
//! - 本文件：字符引用解码（[`decode_entities`] / [`unescape_once`]，全仓唯一一份，表在
//!   [`crate::bili::entities`]）、Kotlin / Java 的空白口径、以及 `HtmlRichText.kt` 的移植
//!   （[`convert`] + [`Hooks`] + [`Builder`]）。
//!
//! # 这一层和 jsoup 的差别
//!
//! 都是**畸形 HTML** 才看得出来（真实正文件一条都不差），
//!
//! - 不实现 HTML5 的"重建活跃格式化元素"（adoption agency）：`<b>x<p>y</p>` 里 jsoup 会在 `p` 里
//!   重建一个 `<b>`，这里让 `b` 一直开着（样式一致，位置不同）；
//! - 不实现表格的 foster parenting（`<table><div>x</div><tr>` 里的 `div` 在 jsoup 里被搬到表格外）；
//! - **树最多 255 层**（[`dom::MAX_DEPTH`]）：更深的嵌套被摊平成兄弟，可见文字一个字节都不丢；
//! - 孤立代理点（`&#xD800;`）：Rust 的 `String` 表示不了，折成 U+FFFD（jsoup 保留半个代理单元）。
//!
//! # 两套"空白"口径，别混用
//!
//! - [`Element::text`] / [`Element::whole_text`]：jsoup 的 `StringUtil` 口径（NBSP 算空白、
//!   `U+3000` 不算、丢 `U+00AD` / `U+200B`、最后按 Java `String.trim()`）；
//! - [`convert`] / [`Builder`]（`HtmlRichText` 的移植）和 [`kotlin_trim`]：**Kotlin 的
//!   `Char.isWhitespace()`**（多认 NBSP / `U+2007` / `U+202F` / `U+0085`）。
//!
//! 这两套都在，注释里互相指了；`zhihu/html.rs` 的 Builder 用的是后者，`Element::text()` 用的是前者。

pub mod dom;

pub use dom::{
    decode_entities, normalize_newlines, parse_body_fragment, to_int_or_null, Element, Node,
};

use std::collections::HashMap;

use crate::detail::{Block, Span};
use crate::json::to_https;

// ===========================================================================
// 字符引用（jsoup Parser.unescapeEntities 的移植）
// ===========================================================================

/// 解**一轮**字符引用（jsoup `Parser.unescapeEntities`）。
///
/// `in_attribute` 为 true 时按属性值的规则：能省分号的 legacy 实体如果后面紧跟字母 / 数字 /
/// `=` / `-` / `_`，就**不解**（浏览器和 jsoup 都是这么做的，`&copy=1` 不能变成 `©=1`）。
/// 实测 jsoup 1.18.3 的阻止集合正好是 `[-0-9=A-Z_a-z]`。
pub fn unescape_once(s: &str, in_attribute: bool) -> String {
    decode_entities(s, in_attribute)
}

/// 最多解三轮，直到不再变化（B站 专栏代码块是两道转义：`&amp;#39;`）。
pub fn unescape_entities_looped(s: &str) -> String {
    let mut cur = s.to_string();
    for _ in 0..3 {
        let next = unescape_once(&cur, false);
        if next == cur {
            return cur;
        }
        cur = next;
    }
    cur
}

// ===========================================================================
// 文本口径（Kotlin / Java 的空白）
// ===========================================================================

/// Kotlin `Char.isWhitespace()` = `Character.isWhitespace(c) || Character.isSpaceChar(c)`：
/// Rust 的 `char::is_whitespace()` 去掉了 U+0085，补上 Java 的 U+001C..U+001F。
pub fn kotlin_is_whitespace(c: char) -> bool {
    match c {
        '\u{1c}'..='\u{1f}' => true,
        '\u{85}' => false,
        _ => c.is_whitespace(),
    }
}

/// Kotlin `String.trim()`（用 [`kotlin_is_whitespace`]）。
pub fn kotlin_trim(s: &str) -> &str {
    kotlin_trim_end(kotlin_trim_start(s))
}

pub fn kotlin_trim_start(s: &str) -> &str {
    let start = s
        .char_indices()
        .find(|(_, c)| !kotlin_is_whitespace(*c))
        .map(|(i, _)| i)
        .unwrap_or(s.len());
    &s[start..]
}

pub fn kotlin_trim_end(s: &str) -> &str {
    let end = s
        .char_indices()
        .rev()
        .find(|(_, c)| !kotlin_is_whitespace(*c))
        .map(|(i, c)| i + c.len_utf8())
        .unwrap_or(0);
    &s[..end]
}

/// Kotlin `String.isBlank()`（空串也算）。
pub fn kotlin_is_blank(s: &str) -> bool {
    s.chars().all(kotlin_is_whitespace)
}

/// Java 正则 `\s`（`HtmlRichText` 的 `WS`）：`[ \t\n\x0B\f\r]`，**不含** NBSP。
fn is_java_regex_space(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\u{0b}' | '\u{0c}' | '\r')
}

/// `Regex("\\s+").replace(s, " ")`：连续 Java 空白压成一个空格（不动行首行尾）。
fn collapse_ws(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_space = false;
    for c in s.chars() {
        if is_java_regex_space(c) {
            if !in_space {
                out.push(' ');
                in_space = true;
            }
        } else {
            out.push(c);
            in_space = false;
        }
    }
    out
}

// ===========================================================================
// 富文本转换（HtmlRichText 的移植）
// ===========================================================================

/// 平台钩子返回的块；`caption` 为 true 表示这是"图片说明"的标记块
/// （Kotlin 那边靠 `IdentityHashMap<Block, Boolean>` 认，这里跟块一起带出来）。
pub struct HookBlock {
    pub block: Block,
    pub caption: bool,
}

impl HookBlock {
    pub fn plain(block: Block) -> Self {
        HookBlock {
            block,
            caption: false,
        }
    }
}

/// 平台特有的钩子。和 Kotlin `HtmlRichText` 的四个闭包一一对应：
/// - `element`：整块替换某个元素（游戏卡片 / 图片说明 / 视频）；`None` = 走默认；
/// - `link`：`<a>` → 行内片段（站内链接 / @）；`None` = 默认的 `Span.Link`；
/// - `inline_image`：行内 `<img>`（表情）→ 行内片段；`None` = 当块级图片；
/// - `image`：块级 `<img>` → 图片块（懒加载属性 / 原图 / 说明）；`None` = 默认的懒加载属性。
pub trait Hooks {
    fn element(&mut self, _el: &Element) -> Option<Vec<HookBlock>> {
        None
    }

    fn link(&mut self, _url: &str, _text: &str, _el: &Element) -> Option<Span> {
        None
    }

    fn inline_image(&mut self, _el: &Element) -> Option<Span> {
        None
    }

    fn image(&mut self, _el: &Element) -> Option<Block> {
        None
    }
}

/// 转换选项（Kotlin `HtmlRichText` 的构造参数）。
pub struct Options {
    pub base_uri: String,
    /// 文本里的 `\n` 当换行（小黑盒的评论 `newlineIsBreak = true`）。
    pub newline_is_break: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            base_uri: String::new(),
            newline_is_break: false,
        }
    }
}

/// 转换结果：`blocks` 是正文；`captions` 是"图片说明标记块"在 `blocks` 里的下标
/// （Kotlin 的 `captions` 集合；调用方拿它把说明并进上一张图）。
pub struct Converted {
    pub blocks: Vec<Block>,
    pub captions: Vec<usize>,
}

/// HTML 片段 → 富文本。
pub fn convert(html: &str, options: &Options, hooks: &mut dyn Hooks) -> Converted {
    let root = parse_body_fragment(html);
    let mut builder = Builder::default();
    let mut walker = Walker {
        hooks,
        base_uri: options.base_uri.clone(),
        newline_is_break: options.newline_is_break,
        captions: Vec::new(),
    };
    walker.run(&root, &mut builder, Style::default());
    builder.flush();
    Converted {
        blocks: builder.out,
        captions: walker.captions,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Paragraph,
    Heading,
    Quote,
}

#[derive(Debug, Clone, Copy)]
struct Style {
    bold: bool,
    italic: bool,
    kind: Kind,
}

impl Default for Style {
    fn default() -> Self {
        Style {
            bold: false,
            italic: false,
            kind: Kind::Paragraph,
        }
    }
}

/// 段落构造器（`HtmlRichText.Builder` 的移植）：文字按样式合并、列表前缀、空段落丢掉。
#[derive(Default)]
pub struct Builder {
    out: Vec<Block>,
    spans: Vec<Span>,
    kind: Option<Kind>,
    prefix: Option<String>,
}

impl Builder {
    pub fn blocks(&self) -> &[Block] {
        &self.out
    }

    fn kind(&self) -> Kind {
        self.kind.unwrap_or(Kind::Paragraph)
    }

    fn next_prefix(&mut self, prefix: String) {
        self.flush();
        self.prefix = Some(prefix);
    }

    fn text(&mut self, t: &str, style: Style) {
        if t.is_empty() {
            return;
        }
        let mut s = t.to_string();
        if self.spans.is_empty() {
            let trimmed = kotlin_trim_start(&s);
            if trimmed.is_empty() {
                return;
            }
            s = trimmed.to_string();
            self.kind = Some(style.kind);
            if let Some(prefix) = self.prefix.take() {
                s = format!("{prefix}{s}");
            }
        }
        if let Some(Span::Text {
            text,
            bold,
            italic,
        }) = self.spans.last_mut()
        {
            if *bold == style.bold && *italic == style.italic {
                let merged = if text.ends_with(' ') && s.starts_with(' ') {
                    format!("{text}{}", kotlin_trim_start(&s))
                } else {
                    format!("{text}{s}")
                };
                *text = merged;
                return;
            }
        }
        self.spans.push(Span::Text {
            text: s,
            bold: style.bold,
            italic: style.italic,
        });
    }

    fn span(&mut self, span: Span, style: Style) {
        if self.spans.is_empty() {
            self.kind = Some(style.kind);
            if let Some(prefix) = self.prefix.take() {
                self.spans.push(Span::Text {
                    text: prefix,
                    bold: false,
                    italic: false,
                });
            }
        }
        self.spans.push(span);
    }

    fn block(&mut self, block: Block) {
        self.flush();
        self.out.push(block);
    }

    fn clear_prefix(&mut self) {
        self.prefix = None;
    }

    fn flush(&mut self) {
        if self.spans.is_empty() {
            return;
        }
        if let Some(Span::Text { text, .. }) = self.spans.last_mut() {
            let trimmed = kotlin_trim_end(text).to_string();
            if trimmed.is_empty() {
                self.spans.pop();
            } else {
                *text = trimmed;
            }
        }
        let keep = self
            .spans
            .iter()
            .any(|s| !matches!(s, Span::Text { text, .. } if kotlin_is_blank(text)));
        if keep {
            let spans = std::mem::take(&mut self.spans);
            self.out.push(match self.kind() {
                Kind::Paragraph => Block::Paragraph { spans },
                Kind::Heading => Block::Heading { spans },
                Kind::Quote => Block::Quote { spans },
            });
        }
        self.spans = Vec::new();
        self.kind = None;
    }
}

struct Walker<'a> {
    hooks: &'a mut dyn Hooks,
    base_uri: String,
    newline_is_break: bool,
    captions: Vec<usize>,
}

/// 一次深度优先遍历的"待办"：显式栈上的每一格。
///
/// 为什么要显式栈：正文可以嵌套很多层（畸形输入几千层），递归的 `element()` 在 256 KB 的线程栈上
/// 会把整个 App 进程带走——Rust 的栈溢出不是 panic，`catch_unwind` 接不住。
/// `zhihu/html.rs` 的 Walker 早就是这么写的（它先被加固过），这里合一之后照同一套来。
enum Step<'a> {
    /// 访问一个节点（文本或元素）。
    Node(&'a Node, Style),
    /// 只处理这一个元素（列表里不是 `li` 的孩子走这里；元素本身要按标签分派）。
    Element(&'a Element, Style),
    /// 只访问这个元素的孩子、不处理元素本身（`<li>` 的项、退回普通内容的 `<a>` 走这里）。
    Children(&'a Element, Style),
    /// 段落收尾（block 级元素结束）。
    Flush,
    /// 列表项前缀。
    Prefix(String),
    /// 列表项收尾：flush + 清前缀。
    EndItem,
}

impl<'a> Walker<'a> {
    /// 跑一棵子树（`stack` 是这一趟的待办栈；调用方先把它清空 / 或直接传一个新的）。
    fn run(&mut self, el: &'a Element, b: &mut Builder, style: Style) {
        let mut stack: Vec<Step<'a>> = Vec::new();
        push_children(&mut stack, el, style);
        while let Some(step) = stack.pop() {
            match step {
                Step::Flush => b.flush(),
                Step::Prefix(prefix) => b.next_prefix(prefix),
                Step::EndItem => {
                    b.flush();
                    b.clear_prefix();
                }
                Step::Children(el, style) => push_children(&mut stack, el, style),
                Step::Element(el, style) => self.element(el, b, style, &mut stack),
                Step::Node(node, style) => match node {
                    Node::Text(text) => self.text(text, b, style),
                    Node::Element(child) => self.element(child, b, style, &mut stack),
                },
            }
        }
    }

    fn text(&mut self, raw: &str, b: &mut Builder, style: Style) {
        if self.newline_is_break && raw.contains('\n') {
            for (i, line) in raw.split('\n').enumerate() {
                if i > 0 {
                    b.flush();
                }
                b.text(&collapse_ws(line), style);
            }
        } else {
            b.text(&collapse_ws(raw), style);
        }
    }

    /// block 级元素：前后各收一次尾。
    fn block_level<'s>(&mut self, el: &'a Element, b: &mut Builder, style: Style, stack: &mut Vec<Step<'a>>) {
        b.flush();
        stack.push(Step::Flush);
        push_children(stack, el, style);
    }

    fn element(&mut self, el: &'a Element, b: &mut Builder, style: Style, stack: &mut Vec<Step<'a>>) {
        if let Some(blocks) = self.hooks.element(el) {
            b.flush();
            for hook in blocks {
                let index = b.out.len();
                b.out.push(hook.block);
                if hook.caption {
                    self.captions.push(index);
                }
            }
            return;
        }
        let tag = el.name();
        match tag {
            "script" | "style" | "noscript" | "template" | "svg" => {}
            "br" => b.flush(),
            "hr" => b.block(Block::Divider),
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => self.block_level(
                el,
                b,
                Style {
                    bold: false,
                    kind: Kind::Heading,
                    ..style
                },
                stack,
            ),
            "blockquote" => self.block_level(
                el,
                b,
                Style {
                    kind: Kind::Quote,
                    ..style
                },
                stack,
            ),
            "pre" => {
                let code = el.select_first("code");
                let mut classes = String::new();
                if let Some(code) = code {
                    classes.push_str(code.class_name());
                }
                classes.push(' ');
                classes.push_str(el.class_name());
                let lang = classes
                    .split(' ')
                    .find_map(|c| c.strip_prefix("language-").filter(|rest| !rest.is_empty()))
                    .map(|s| s.to_string())
                    .or_else(|| {
                        let lang = el.attr("lang");
                        if lang.is_empty() {
                            None
                        } else {
                            Some(lang.to_string())
                        }
                    });
                // `wholeText().trimEnd('\n')`
                let code_text = el.whole_text();
                let trimmed = code_text.trim_end_matches('\n').to_string();
                b.block(Block::Code {
                    text: trimmed,
                    language: lang,
                });
            }
            "ul" | "ol" => {
                b.flush();
                let mut i = el.attr("start").parse::<i32>().ok().unwrap_or(1);
                let mut items: Vec<Step<'a>> = Vec::new();
                for li in el.child_elements() {
                    if li.name() != "li" {
                        items.push(Step::Element(li, style));
                        continue;
                    }
                    if tag == "ol" {
                        items.push(Step::Prefix(format!("{i}. ")));
                        i += 1;
                    } else {
                        items.push(Step::Prefix("• ".to_string()));
                    }
                    items.push(Step::Children(li, style));
                    items.push(Step::EndItem);
                }
                for step in items.into_iter().rev() {
                    stack.push(step);
                }
            }
            "table" => {
                b.flush();
                for tr in el.select("tr") {
                    let row = tr
                        .child_elements()
                        .map(|c| c.text().trim().to_string())
                        .collect::<Vec<_>>()
                        .join(" | ");
                    if !kotlin_is_blank(&row) {
                        b.block(Block::Paragraph {
                            spans: vec![Span::Text {
                                text: row,
                                bold: false,
                                italic: false,
                            }],
                        });
                    }
                }
            }
            "figure" => {
                let img = el.select_first("img");
                let caption = el
                    .select_first("figcaption")
                    .map(|c| c.text().trim().to_string())
                    .filter(|c| !c.is_empty());
                match img {
                    None => self.block_level(el, b, style, stack),
                    Some(img) => {
                        if let Some(mut image) = self.block_image(img) {
                            if let Some(caption) = caption {
                                if let Block::Image { caption: slot, .. } = &mut image {
                                    *slot = Some(caption);
                                }
                            }
                            b.block(image);
                        }
                    }
                }
            }
            "figcaption" => self.block_level(el, b, style, stack),
            "img" => {
                let emote = self.hooks.inline_image(el);
                match emote {
                    Some(span) => b.span(span, style),
                    None => {
                        if let Some(image) = self.block_image(el) {
                            b.block(image);
                        }
                    }
                }
            }
            "a" => match self.anchor(el) {
                None => push_children(stack, el, style),
                Some(Some(span)) => b.span(span, style),
                Some(None) => {}
            },
            "b" | "strong" => push_children(
                stack,
                el,
                Style {
                    bold: true,
                    ..style
                },
            ),
            "i" | "em" => push_children(
                stack,
                el,
                Style {
                    italic: true,
                    ..style
                },
            ),
            _ if is_block_level_tag(tag) => self.block_level(el, b, style, stack),
            _ => push_children(stack, el, style),
        }
    }

    /// `<a>`：能做成行内链接就给一个片段，否则按普通内容处理（`<a>` 包着图片 / 没有地址 / `javascript:`）。
    ///
    /// 返回值：`None` = 去访问它的孩子；`Some(None)` = 什么都不输出；`Some(Some(span))` = 一个行内片段。
    fn anchor(&mut self, el: &Element) -> Option<Option<Span>> {
        let href = el.attr("href").trim().to_string();
        let text = el.text().trim().to_string();
        if el.select_first("img").is_some()
            || href.is_empty()
            || href.to_ascii_lowercase().starts_with("javascript:")
        {
            return None;
        }
        let abs = el.abs_url(&self.base_uri, "href");
        let url = to_https(if abs.is_empty() { &href } else { &abs });
        let span = self.hooks.link(&url, &text, el).or_else(|| {
            if text.is_empty() {
                None
            } else {
                Some(Span::Link {
                    text: text.clone(),
                    url: url.clone(),
                    target: None,
                })
            }
        });
        Some(span.map(web_link_only))
    }

    fn block_image(&mut self, el: &Element) -> Option<Block> {
        if let Some(image) = self.hooks.image(el) {
            return match &image {
                Block::Image { url, .. } if !is_web_url(url) => None,
                _ => Some(image),
            };
        }
        let src = ["data-actualsrc", "data-src", "src"]
            .iter()
            .find_map(|attr| {
                // jsoup `absUrl`：属性**在**、值空 → baseUri；属性不在 → 空串。先判 `has_attr`。
                if !el.has_attr(attr) {
                    return None;
                }
                let abs = el.abs_url(&self.base_uri, attr);
                if !kotlin_is_blank(&abs) && is_web_url(&abs) {
                    Some(abs)
                } else {
                    None
                }
            })?;
        let original = {
            let raw = el.attr("data-original");
            if !kotlin_is_blank(raw) && raw != src {
                let abs = el.abs_url(&self.base_uri, "data-original");
                Some(to_https(if abs.is_empty() { raw } else { &abs }))
            } else {
                None
            }
        };
        Some(Block::Image {
            url: to_https(&src),
            width: parse_int_attr(el.attr("width")),
            height: parse_int_attr(el.attr("height")),
            original,
            caption: None,
        })
    }
}

/// 正文里的链接 / 图片只认网页地址：`http(s)://` 和协议相对的 `//`。
/// 正文是第三方写的：`file://` / `content://` / `intent:` 之类交给系统会崩、或者读本机文件。
pub fn is_web_url(url: &str) -> bool {
    let t = url.trim_start();
    let lower = t.get(..8).unwrap_or(t).to_ascii_lowercase();
    lower.starts_with("https://") || lower.starts_with("http://") || lower.starts_with("//")
}

/// 不是网页地址、又没有站内目标的链接降成纯文字（站内目标走原生打开，不交给系统）。
fn web_link_only(span: Span) -> Span {
    match span {
        Span::Link { text, url, target: None } if !is_web_url(&url) => Span::Text {
            text,
            bold: false,
            italic: false,
        },
        other => other,
    }
}

/// 把一个元素的孩子按文档顺序压进待办栈（逆序入栈，弹出来就是正序）。
fn push_children<'a>(stack: &mut Vec<Step<'a>>, el: &'a Element, style: Style) {
    for node in el.child_nodes().iter().rev() {
        stack.push(Step::Node(node, style));
    }
}

/// Kotlin `String.toIntOrNull()`：可选正负号 + 十进制数字，**不 trim**、不认空格。
fn parse_int_attr(raw: &str) -> Option<i32> {
    if raw.is_empty() {
        return None;
    }
    let mut chars = raw.chars().peekable();
    let mut sign = 1i64;
    if let Some(&c) = chars.peek() {
        if c == '+' || c == '-' {
            if c == '-' {
                sign = -1;
            }
            chars.next();
        }
    }
    let digits: String = chars.take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        return None;
    }
    digits
        .parse::<i64>()
        .ok()
        .map(|v| sign * v)
        .and_then(|v| i32::try_from(v).ok())
}

/// `HtmlRichText.BLOCK_TAGS`：这些标签默认分段。
fn is_block_level_tag(tag: &str) -> bool {
    matches!(
        tag,
        "p" | "div"
            | "section"
            | "article"
            | "header"
            | "footer"
            | "main"
            | "aside"
            | "nav"
            | "li"
            | "dl"
            | "dt"
            | "dd"
            | "address"
            | "details"
            | "summary"
            | "center"
    )
}

// ===========================================================================
// 小工具：小黑盒正文里的"图片配对键"
// ===========================================================================

/// `HeyboxBody.imageKey`：去掉协议、域名和 query，只留路径（HTML 里的 `data-original` 和
/// 段数组里的 `img` 是同一张图，用它对上）。
pub fn image_key(url: &str) -> String {
    let no_query = url.split('?').next().unwrap_or("");
    let after_scheme = match no_query.split_once("://") {
        Some((_, rest)) => rest,
        None => no_query,
    };
    match after_scheme.split_once('/') {
        Some((_, path)) => path.to_string(),
        None => String::new(),
    }
}

/// 缓存：`HashMap` 够用（键是图片路径）。
pub type ThumbMap<'a> = HashMap<String, &'a crate::json::JsonMap>;

// ===========================================================================
// `core/Text.kt` 的 `stripHtml`（搜索结果的标题 / 摘要）
// ===========================================================================

/// `core/Text.kt` 的 `stripHtml`：`<br>` / `</p>` 换成换行、其他标签去掉、六个常见实体还原、
/// 连续 3 个以上换行收成 2 个、最后 trim。
///
/// **为什么不用 [`parse_body_fragment`] + [`Element::text`]**：Kotlin 侧这条是**一串正则替换**
/// （`<br\s*/?>` → 换行、`</p>` → 换行、`<[^>]+>` 删掉），和 jsoup 的建树口径并不一样
/// （`<brfoo>`、未闭合的 `<script>` 这类才看得出来）。搜索标题 / 摘要里只有高亮标签，
/// 用正则口径反而和 Kotlin 逐字节一致。trim 用 [`kotlin_trim`]（Kotlin 的 `Char.isWhitespace` 含 NBSP 类）。
pub fn strip_html(html: &str) -> String {
    let s = replace_br(html, "\n");
    let s = replace_ci(&s, "</p>", "\n");
    let s = drop_tags(&s);
    let s = s
        .replace("&nbsp;", " ")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&amp;", "&");
    let s = collapse_newlines(&s, 2);
    kotlin_trim(&s).to_string()
}

/// `core/Text.kt` 的 `stripHtml` **再加上**搜索那两家共用的空白口径：
/// `Regex("[ \\t\\u3000]{2,}")` → 一个半角空格，最后 trim。
///
/// B站（`BiliSearchParser.clean`）和贴吧（`TiebaSearch.cleanText`）**是同一行代码**，所以只写一份。
pub fn clean_search_text(html: &str) -> String {
    let stripped = strip_html(html);
    let chars: Vec<char> = stripped.chars().collect();
    let mut out = String::with_capacity(stripped.len());
    let mut i = 0usize;
    while i < chars.len() {
        if matches!(chars[i], ' ' | '\t' | '\u{3000}') {
            let mut j = i;
            while j < chars.len() && matches!(chars[j], ' ' | '\t' | '\u{3000}') {
                j += 1;
            }
            if j - i >= 2 {
                out.push(' ');
            } else {
                out.push(chars[i]);
            }
            i = j;
            continue;
        }
        out.push(chars[i]);
        i += 1;
    }
    kotlin_trim(&out).to_string()
}

/// `<br\s*/?>`（忽略大小写）→ [`replacement`]。`\s` 是 Java 正则的六个 ASCII 空白。
fn replace_br(s: &str, replacement: &str) -> String {
    let b = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'<' {
            let mut j = i + 1;
            if j + 1 < b.len()
                && b[j].eq_ignore_ascii_case(&b'b')
                && b[j + 1].eq_ignore_ascii_case(&b'r')
            {
                j += 2;
                while j < b.len() && is_java_regex_space(b[j] as char) {
                    j += 1;
                }
                if j < b.len() && b[j] == b'/' {
                    j += 1;
                }
                if j < b.len() && b[j] == b'>' {
                    out.push_str(replacement);
                    i = j + 1;
                    continue;
                }
            }
        }
        let len = if b[i] >= 0x80 { utf8_len(b[i]) } else { 1 };
        out.push_str(&s[i..i + len]);
        i += len;
    }
    out
}

/// 忽略大小写地把 [`needle`] 换成 [`replacement`]（Java 的 `Regex(needle, IGNORE_CASE).replace`）。
fn replace_ci(s: &str, needle: &str, replacement: &str) -> String {
    let b = s.as_bytes();
    let n = needle.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < b.len() {
        if i + n.len() <= b.len()
            && b[i..i + n.len()]
                .iter()
                .zip(n.iter())
                .all(|(x, y)| x.eq_ignore_ascii_case(y))
        {
            out.push_str(replacement);
            i += n.len();
            continue;
        }
        let len = if b[i] >= 0x80 { utf8_len(b[i]) } else { 1 };
        out.push_str(&s[i..i + len]);
        i += len;
    }
    out
}

/// `<[^>]+>`：至少一个字符的一对尖括号（`<>` 不是标签，原样留下）。
fn drop_tags(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'<' {
            if let Some(rel) = s[i..].find('>') {
                let end = i + rel;
                if end > i + 1 {
                    i = end + 1;
                    continue;
                }
            }
        }
        let len = if b[i] >= 0x80 { utf8_len(b[i]) } else { 1 };
        out.push_str(&s[i..i + len]);
        i += len;
    }
    out
}

/// `\n{limit+1,}` → `\n` × `limit`（Kotlin 是 `Regex("\n{3,}")` → `"\n\n"`，即 limit = 2）。
fn collapse_newlines(s: &str, limit: usize) -> String {
    let mut out = String::with_capacity(s.len());
    let mut run = 0usize;
    for c in s.chars() {
        if c == '\n' {
            run += 1;
            if run <= limit {
                out.push(c);
            }
        } else {
            run = 0;
            out.push(c);
        }
    }
    out
}

/// UTF-8 前导字节 → 这个字符占几个字节（非法字节当 1，不会越界）。
pub fn utf8_len(first: u8) -> usize {
    match first {
        0xF0..=0xFF => 4,
        0xE0..=0xEF => 3,
        0xC0..=0xDF => 2,
        _ => 1,
    }
}

