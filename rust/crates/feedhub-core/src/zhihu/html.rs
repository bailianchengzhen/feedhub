//! 知乎正文 / 评论 HTML → [`RichText`]：`core/HtmlRichText.kt` 的通用规则 + `source-zhihu/ZhihuHtml.kt`
//! 的知乎专有结构，逐条对着 Kotlin 复刻。
//!
//! 通用规则（`HtmlRichText`）：p / div / h1~h6 / blockquote / li 分段；`pre` → `Block::Code`；
//! `hr` → `Block::Divider`；ul / ol 每项加 `• ` / `1. ` 前缀；table 每行一段、单元格用 ` | ` 连；
//! figure 里的 img + figcaption → 带说明的图片；b / strong、i / em 变粗 / 斜；script / style / noscript /
//! template / svg 丢掉；空段落不输出，连续空白压成一个空格。
//!
//! 知乎自己那几样（`ZhihuHtml`）：
//! - 图片懒加载：显示地址取 `data-actualsrc` / `src`，原图 `data-original`，宽高 `data-rawwidth` / `data-rawheight`；
//!   GIF 不给原图（`data-original` 是静态首帧）；
//! - 公式 `<img eeimg>` → 斜体的 LaTeX 文字（alt）；
//! - `@人` → `Span::Mention`；`link.zhihu.com/?target=` 解开；站内回答 / 文章 / 想法 / 视频 / 问题 →
//!   `DetailTarget::Native`；
//! - 视频盒子 `<a class="video-box" data-lens-id>` → `Block::Video`（没有 lens id 就退回卡片）；
//!   链接卡片 `<a data-draft-type="link-card">` → `Block::Card`（电商插件丢掉）；
//! - 脚注 `<sup data-draft-type="reference">`：正文里保留 `[n]`，文末补一段"参考"，按**首次出现顺序**；
//! - 评论里的图片 `<a class="comment_img|comment_gif|comment_sticker">` → `Block::Image`；
//! - 零宽字符（U+200B~U+200D、U+2060、U+FEFF）去掉；有表情表时把 `[感谢]` 换成 `Span::Emote`。
//!
//! HTML 本身由共用的 [`crate::html::dom`] 解析；
//! 树上的 jsoup 空白口径（`Element::text()` 等）也在那里。

use crate::html::dom::Element;
use super::links;
use crate::detail::{Block, RichText, Span};
use crate::hosted::is_java_whitespace;
use crate::json::to_https;
use crate::model::UserRef;

/// 知乎的正文基址（jsoup `parseBodyFragment` 的 baseUri，用来补全相对地址）。
const BASE_URI: &str = "https://www.zhihu.com/";

/// 正文 / 评论的 HTML → 富文本。
///
/// `emotes` 是表情表（占位符含方括号 → 图片地址），空表示不换表情；
/// `newline_is_break` 对应 Kotlin 的 `newlineIsBreak`（想法、视频简介里的纯文本换行当段落）。
pub fn parse(html: &str, emotes: &[(String, String)], newline_is_break: bool) -> RichText {
    let root = crate::html::dom::parse_body_fragment(html);
    let mut ctx = Ctx {
        refs: Vec::new(),
        newline_is_break,
    };
    let mut builder = Builder::new();
    walk_children(&root, &mut builder, &Style::default(), &mut ctx);
    builder.flush();
    let mut blocks = finish(builder.out, emotes);
    if !ctx.refs.is_empty() {
        blocks.push(Block::Heading {
            spans: vec![Span::Text {
                text: "参考".to_string(),
                bold: false,
                italic: false,
            }],
        });
        for reference in &ctx.refs {
            blocks.push(reference_block(reference));
        }
    }
    RichText { blocks }
}

/// 评论 / 想法正文里有没有 `[xxx]` 形式的表情占位符（没有就不必去取表情表）。
pub fn has_emote_candidate(text: &str) -> bool {
    find_emotes(text).next().is_some()
}

// ===========================================================================
// 构建
// ===========================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Paragraph,
    Heading,
    Quote,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

struct Ctx {
    /// 脚注：按 `data-numero` 去重，保留**首次出现**的顺序（Kotlin 是 `linkedMapOf`）。
    refs: Vec<Reference>,
    /// Kotlin `HtmlRichText` 的构造参数 `newlineIsBreak`：纯文本里的 `\n` 当分段。
    newline_is_break: bool,
}

struct Reference {
    numero: String,
    text: String,
    url: String,
}

struct Builder {
    out: Vec<Block>,
    spans: Vec<Span>,
    kind: Kind,
    prefix: Option<String>,
}

impl Builder {
    fn new() -> Self {
        Builder {
            out: Vec::new(),
            spans: Vec::new(),
            kind: Kind::Paragraph,
            prefix: None,
        }
    }

    fn next_prefix(&mut self, prefix: &str) {
        self.flush();
        self.prefix = Some(prefix.to_string());
    }

    fn clear_prefix(&mut self) {
        self.prefix = None;
    }

    fn text(&mut self, text: &str, style: Style) {
        if text.is_empty() {
            return;
        }
        let mut s = text.to_string();
        if self.spans.is_empty() {
            s = s.trim_start_matches(is_kotlin_whitespace).to_string();
            if s.is_empty() {
                return;
            }
            self.kind = style.kind;
            if let Some(prefix) = self.prefix.take() {
                s = format!("{prefix}{s}");
            }
        }
        if let Some(Span::Text {
            text: last,
            bold,
            italic,
        }) = self.spans.last_mut()
        {
            if *bold == style.bold && *italic == style.italic {
                // 相邻同样式的文字合并，空白不重复。**就地追加**（原来 `format!` 每次都把已经攒好的
                // 字符串整份复制一遍：一个段落里几万个文本节点就退化成 O(n²)）。
                if last.ends_with(' ') && s.starts_with(' ') {
                    let rest = s.trim_start_matches(is_kotlin_whitespace);
                    last.push_str(rest);
                } else {
                    last.push_str(&s);
                }
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
            self.kind = style.kind;
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

    /// 结束当前段落。空段落什么都不做（也不吃掉列表前缀）。
    fn flush(&mut self) {
        if self.spans.is_empty() {
            return;
        }
        if let Some(Span::Text {
            text,
            bold,
            italic,
        }) = self.spans.last_mut()
        {
            let trimmed = text.trim_end_matches(is_kotlin_whitespace).to_string();
            if trimmed.is_empty() {
                self.spans.pop();
            } else {
                *text = trimmed;
                let _ = (bold, italic);
            }
        }
        let keep = self
            .spans
            .iter()
            .any(|s| !matches!(s, Span::Text { text, .. } if is_blank(text)));
        if keep {
            let spans = std::mem::take(&mut self.spans);
            self.out.push(match self.kind {
                Kind::Paragraph => Block::Paragraph { spans },
                Kind::Heading => Block::Heading { spans },
                Kind::Quote => Block::Quote { spans },
            });
        } else {
            self.spans.clear();
        }
        self.kind = Kind::Paragraph;
    }
}

/// **Kotlin 的 `Char.isWhitespace()`**（正文 / 评论的 Builder 用的是 Kotlin 的 `trimEnd()` / `trimStart()`，
/// 不是 Java 的 `String.trim()`）。
///
/// 和 [`is_java_whitespace`] 的差别就是那四个：`U+00A0`（NBSP）、`U+2007`、`U+202F`、`U+0085`——
/// **Kotlin 把 NBSP 算空白，Java 不算**。这个口径是差分测试里"空白"那一大类的根，
///
/// 注意：`dom.rs` 里 `Element::text()` / `whole_text()` 的口径**不同**（那是 jsoup 的 `StringUtil`，
/// NBSP 算空白但 `U+3000` 不算），别混用。
fn is_kotlin_whitespace(c: char) -> bool {
    is_java_whitespace(c) || matches!(c, '\u{a0}' | '\u{2007}' | '\u{202f}' | '\u{85}')
}

fn is_blank(s: &str) -> bool {
    s.chars().all(is_kotlin_whitespace)
}

/// Kotlin 的 `Regex("\\s+")`（Java 的 `\s` 只认 ASCII 空白）。
fn collapse_whitespace(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_white = false;
    for c in text.chars() {
        if matches!(c, ' ' | '\t' | '\n' | '\u{b}' | '\u{c}' | '\r') {
            in_white = true;
        } else {
            if in_white {
                out.push(' ');
                in_white = false;
            }
            out.push(c);
        }
    }
    if in_white {
        out.push(' ');
    }
    out
}

/// 一次深度优先遍历的"待办"：显式栈上的每一格。
///
/// 为什么要显式栈：正文可以嵌套很多层（畸形输入几千层），递归的 `element()` 在 256 KB 的线程栈上
/// 会把整个 App 进程带走——Rust 的栈溢出不是 panic，`catch_unwind` 接不住。
enum Step<'a> {
    /// 访问一个节点（文本或元素）。
    Node(&'a crate::html::dom::Node, Style),
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

fn walk_children(el: &Element, b: &mut Builder, style: &Style, ctx: &mut Ctx) {
    let mut stack: Vec<Step<'_>> = Vec::new();
    push_children(&mut stack, el, *style);
    while let Some(step) = stack.pop() {
        match step {
            Step::Flush => b.flush(),
            Step::Prefix(prefix) => b.next_prefix(&prefix),
            Step::EndItem => {
                b.flush();
                b.clear_prefix();
            }
            Step::Children(el, style) => push_children(&mut stack, el, style),
            Step::Element(el, style) => element(el, b, &style, ctx, &mut stack),
            Step::Node(node, style) => match node {
                crate::html::dom::Node::Text(text) => text_node(text, b, &style, ctx),
                crate::html::dom::Node::Element(child) => element(child, b, &style, ctx, &mut stack),
            },
        }
    }
}

/// 把一个元素的孩子按文档顺序压进待办栈（逆序入栈，弹出来就是正序）。
fn push_children<'a>(stack: &mut Vec<Step<'a>>, el: &'a Element, style: Style) {
    for node in el.children.iter().rev() {
        stack.push(Step::Node(node, style));
    }
}

fn text_node(raw: &str, b: &mut Builder, style: &Style, ctx: &mut Ctx) {
    if ctx.newline_is_break && raw.contains('\n') {
        for (i, line) in raw.split('\n').enumerate() {
            if i > 0 {
                b.flush();
            }
            b.text(&collapse_whitespace(line), *style);
        }
    } else {
        b.text(&collapse_whitespace(raw), *style);
    }
}

fn element<'a>(el: &'a Element, b: &mut Builder, style: &Style, ctx: &mut Ctx, stack: &mut Vec<Step<'a>>) {
    if let Some(blocks) = element_hook(el, ctx) {
        b.flush();
        for block in blocks {
            b.block(block);
        }
        return;
    }
    match el.name.as_str() {
        "script" | "style" | "noscript" | "template" | "svg" => {}
        "br" => b.flush(),
        "hr" => b.block(Block::Divider),
        "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => block_level(
            el,
            b,
            Style {
                bold: false,
                kind: Kind::Heading,
                ..*style
            },
            stack,
        ),
        "blockquote" => block_level(
            el,
            b,
            Style {
                kind: Kind::Quote,
                ..*style
            },
            stack,
        ),
        "pre" => {
            let code = el.select_first("code");
            let lang = language(el, code);
            b.block(Block::Code {
                text: el.whole_text().trim_end_matches('\n').to_string(),
                language: lang,
            });
        }
        "ul" | "ol" => {
            b.flush();
            let mut index = el.attr_int("start").unwrap_or(1);
            let mut items: Vec<Step<'a>> = Vec::new();
            for child in el.child_elements() {
                if child.name != "li" {
                    items.push(Step::Element(child, *style));
                    continue;
                }
                if el.name == "ol" {
                    items.push(Step::Prefix(format!("{index}. ")));
                    index += 1;
                } else {
                    items.push(Step::Prefix("• ".to_string()));
                }
                items.push(Step::Children(child, *style));
                items.push(Step::EndItem);
            }
            for step in items.into_iter().rev() {
                stack.push(step);
            }
        }
        "table" => {
            b.flush();
            let mut rows: Vec<&Element> = Vec::new();
            el.select_all("tr", &mut rows);
            for row in rows {
                let cells: Vec<String> = row.child_elements().map(|cell| cell.text().trim().to_string()).collect();
                let row_text = cells.join(" | ");
                if !row_text.trim().is_empty() {
                    b.block(Block::Paragraph {
                        spans: vec![Span::Text {
                            text: row_text,
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
                .filter(|s| !s.is_empty());
            match img {
                None => block_level(el, b, *style, stack),
                Some(img) => {
                    if let Some(mut image) = block_image(img) {
                        if caption.is_some() {
                            if let Block::Image { caption: c, .. } = &mut image {
                                *c = caption;
                            }
                        }
                        b.block(image);
                    }
                }
            }
        }
        "figcaption" => block_level(el, b, *style, stack),
        "img" => {
            if let Some(emote) = inline_image(el) {
                b.span(emote, *style);
            } else if let Some(image) = block_image(el) {
                b.block(image);
            }
        }
        "a" => match anchor(el) {
            None => push_children(stack, el, *style),
            Some(Some(span)) => b.span(span, *style),
            Some(None) => {}
        },
        "b" | "strong" => push_children(
            stack,
            el,
            Style {
                bold: true,
                ..*style
            },
        ),
        "i" | "em" => push_children(
            stack,
            el,
            Style {
                italic: true,
                ..*style
            },
        ),
        name if BLOCK_TAGS.contains(&name) => block_level(el, b, *style, stack),
        _ => push_children(stack, el, *style),
    }
}

/// block 级元素：前后各收一次尾。
fn block_level<'a>(el: &'a Element, b: &mut Builder, style: Style, stack: &mut Vec<Step<'a>>) {
    b.flush();
    stack.push(Step::Flush);
    push_children(stack, el, style);
}

fn language(pre: &Element, code: Option<&Element>) -> Option<String> {
    let combined = format!(
        "{} {}",
        code.map(|c| c.class_name()).unwrap_or(""),
        pre.class_name()
    );
    for token in combined.split(' ') {
        if let Some(rest) = token.strip_prefix("language-") {
            if !rest.is_empty() {
                return Some(rest.to_string());
            }
        }
    }
    let lang = pre.attr("lang");
    if lang.is_empty() {
        None
    } else {
        Some(lang.to_string())
    }
}

/// `<a>`：能做成行内链接就给一个片段，否则按普通内容处理（`<a>` 包着图片 / 没有地址 / `javascript:`）。
///
/// 返回值：`None` = 去访问它的孩子；`Some(None)` = 什么都不输出；`Some(Some(span))` = 一个行内片段。
fn anchor(el: &Element) -> Option<Option<Span>> {
    let href = el.attr("href").trim();
    // 包着图片、或者没有地址的 <a>：按普通内容处理
    if el.select_first("img").is_some() || href.is_empty() || href.to_ascii_lowercase().starts_with("javascript:") {
        return None;
    }
    let text = el.text().trim().to_string();
    let resolved = absolute_url(href);
    let url = to_https(&resolved);
    let span = match link_span(&url, &text, el) {
        Some(span) => span,
        None => {
            if text.is_empty() {
                return Some(None);
            }
            let target = links::target(&url);
            Span::Link { text, url, target }
        }
    };
    Some(Some(span))
}

fn block_image(el: &Element) -> Option<Block> {
    if let Some(image) = zhihu_image(el) {
        return Some(image);
    }
    let mut src = None;
    for attr in ["data-actualsrc", "data-src", "src"] {
        // jsoup `absUrl` 只看**属性在不在**：`<img src>`（属性空）解析成 baseUri，
        // `<img>`（没这个属性）是空串。两种都要和 Kotlin 一致，所以先判 `has_attr`。
        if !el.has_attr(attr) {
            continue;
        }
        let value = absolute_url(&el.attr(attr));
        if !value.trim().is_empty() && !value.starts_with("data:") {
            src = Some(value);
            break;
        }
    }
    let src = src?;
    let original_attr = el.attr("data-original");
    let original = if el.has_attr("data-original") && !original_attr.trim().is_empty() && original_attr != src {
        Some(absolute_url(original_attr))
    } else {
        None
    };
    Some(Block::Image {
        url: to_https(&src),
        width: el.attr_int("width"),
        height: el.attr_int("height"),
        original: original.map(|o| to_https(&o)),
        caption: None,
    })
}

// ===========================================================================
// 知乎的钩子（ZhihuHtml.kt）
// ===========================================================================

fn element_hook(el: &Element, ctx: &mut Ctx) -> Option<Vec<Block>> {
    match el.name.as_str() {
        "a" => {
            if el.has_class("video-box") {
                return Some(vec![video_card(el)]);
            }
            if el.attr("data-draft-type") == "link-card" {
                return Some(link_card(el));
            }
            if el.has_class("comment_img") || el.has_class("comment_gif") || el.has_class("comment_sticker") {
                return Some(comment_image(el).into_iter().collect());
            }
            None
        }
        "sup" => {
            // 只登记脚注，返回 None 走默认（正文里保留 [1]）
            if el.attr("data-draft-type") == "reference" {
                let numero = el.attr("data-numero").trim();
                let text = el.attr("data-text").trim();
                let url = el.attr("data-url").trim();
                if !numero.is_empty() && (!text.is_empty() || !url.is_empty()) {
                    if !ctx.refs.iter().any(|r| r.numero == numero) {
                        ctx.refs.push(Reference {
                            numero: numero.to_string(),
                            text: text.to_string(),
                            url: url.to_string(),
                        });
                    }
                }
            }
            None
        }
        _ => None,
    }
}

/// 视频盒子：有 lens id → 行内播放的 `Block::Video`；没有就退回卡片。
fn video_card(el: &Element) -> Block {
    let url = {
        let unwrapped = links::unwrap(el.attr("href"));
        let https = to_https(&unwrapped);
        if is_blank(&https) {
            None
        } else {
            Some(https)
        }
    };
    let mut title = el.attr("data-name").trim().to_string();
    if title.trim().is_empty() {
        title = el
            .select_first_class("span", "title")
            .map(|span| span.own_text().trim().to_string())
            .unwrap_or_default();
    }
    if title.trim().is_empty() {
        title = "视频".to_string();
    }
    let mut cover = el.attr("data-poster").trim().to_string();
    if cover.is_empty() {
        cover = el.select_first("img").map(|img| img.attr("src").to_string()).unwrap_or_default();
    }
    let cover = if cover.is_empty() { None } else { Some(to_https(&cover)) };
    let lens = {
        let attr = el.attr("data-lens-id").trim();
        if !attr.is_empty() && attr.chars().all(|c| c.is_ascii_digit()) {
            Some(attr.to_string())
        } else {
            url.as_deref()
                .and_then(|u| u.split_once("/video/").map(|(_, rest)| rest))
                .map(|rest| rest.split('?').next().unwrap_or("").to_string())
                .filter(|s| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit()))
        }
    };
    match lens {
        Some(lens) => Block::Video {
            id: lens.clone(),
            part_id: lens,
            cover,
            duration_sec: None,
            width: None,
            height: None,
            title: if title == "视频" { None } else { Some(title) },
            url,
        },
        None => Block::Card {
            title,
            subtitle: Some("视频".to_string()),
            cover,
            url,
            target: None,
        },
    }
}

fn link_card(el: &Element) -> Vec<Block> {
    let url = to_https(&links::unwrap(el.attr("href").trim()));
    let Some(parsed) = url::parse(&url) else {
        return Vec::new();
    };
    // 商品插件（BIZ=ECOMMERCE）是推广，不要
    if parsed.host == "xg.zhihu.com" || parsed.query_param("BIZ").as_deref() == Some("ECOMMERCE") {
        return Vec::new();
    }
    let mut title = el.attr("data-draft-title").trim().to_string();
    if title.is_empty() {
        title = el.text().trim().to_string();
    }
    if title.is_empty() {
        title = url.clone();
    }
    let cover = {
        let value = el.attr("data-draft-cover").trim();
        if value.is_empty() {
            None
        } else {
            Some(to_https(value))
        }
    };
    let subtitle = if title.contains(parsed.host.as_str()) {
        None
    } else {
        Some(parsed.host.clone())
    };
    vec![Block::Card {
        title,
        subtitle,
        cover,
        target: links::target(&url),
        url: Some(url),
    }]
}

fn comment_image(el: &Element) -> Option<Block> {
    let href = el.attr("href").trim();
    if href.is_empty() {
        return None;
    }
    Some(Block::Image {
        url: to_https(href),
        width: el.attr_int("data-width"),
        height: el.attr_int("data-height"),
        original: None,
        caption: None,
    })
}

/// `link` 钩子：@人 → Mention；别的（有文字）→ 链接。
fn link_span(url: &str, text: &str, el: &Element) -> Option<Span> {
    if el.has_class("member_mention") {
        let id = {
            let attr = el.attr("data-hash");
            if is_blank(attr) {
                url.rsplit('/').next().unwrap_or("").split('?').next().unwrap_or("").to_string()
            } else {
                attr.trim().to_string()
            }
        };
        if !is_blank(&id) {
            return Some(Span::Mention {
                text: text.to_string(),
                user: UserRef {
                    platform: super::ZHIHU.to_string(),
                    id: id.clone(),
                    name: text.strip_prefix('@').unwrap_or(text).to_string(),
                    avatar: None,
                    url: Some(super::people_url(&id)),
                },
            });
        }
    }
    if text.is_empty() {
        return None;
    }
    // 和 Kotlin 一样：`link()` 里再解开一次 link.zhihu.com 跳转（外面的 absUrl 那步不认跳转页）
    let url = to_https(&links::unwrap(url));
    Some(Span::Link {
        text: text.to_string(),
        target: links::target(&url),
        url,
    })
}

/// 行内 `<img>`：公式（`eeimg`）→ 斜体 LaTeX；评论里的表情图 → `Span::Emote`。
fn inline_image(el: &Element) -> Option<Span> {
    if el.has_attr("eeimg") {
        let tex = el.attr("alt").trim();
        if tex.is_empty() {
            return None;
        }
        return Some(Span::Text {
            text: tex.to_string(),
            bold: false,
            italic: true,
        });
    }
    let alt = el.attr("alt").trim();
    let src = el.attr("src").trim();
    if is_emote(alt) && !src.is_empty() && !src.starts_with("data:") && el.class_name().contains("sticker") {
        return Some(Span::Emote {
            text: alt.to_string(),
            url: to_https(src),
        });
    }
    None
}

/// 块级 `<img>` 的知乎口径：懒加载的 `data-actualsrc` 优先，`data-original` 是原图（GIF 除外）。
fn zhihu_image(el: &Element) -> Option<Block> {
    let mut src = None;
    for attr in ["data-actualsrc", "src"] {
        let value = el.attr(attr).trim();
        if !value.is_empty() && !value.starts_with("data:") {
            src = Some(to_https(value));
            break;
        }
    }
    let src = src?;
    let original = {
        let value = el.attr("data-original").trim();
        if value.is_empty() {
            None
        } else {
            Some(to_https(value))
        }
    };
    let original = original.filter(|o| {
        *o != src && !src.split('?').next().unwrap_or("").ends_with(".gif")
    });
    Some(Block::Image {
        url: src,
        width: el.attr_int("data-rawwidth").or_else(|| el.attr_int("width")),
        height: el.attr_int("data-rawheight").or_else(|| el.attr_int("height")),
        original,
        caption: {
            let value = el.attr("data-caption").trim();
            if value.is_empty() {
                None
            } else {
                Some(value.to_string())
            }
        },
    })
}

fn reference_block(r: &Reference) -> Block {
    let mut spans = vec![Span::Text {
        text: format!("[{}] {}", r.numero, r.text)
            .trim_end_matches(is_kotlin_whitespace)
            .to_string(),
        bold: false,
        italic: false,
    }];
    if !r.url.is_empty() {
        let url = to_https(&links::unwrap(&r.url));
        spans.push(Span::Text {
            text: " ".to_string(),
            bold: false,
            italic: false,
        });
        spans.push(Span::Link {
            text: url.clone(),
            url: url.clone(),
            target: links::target(&url),
        });
    }
    Block::Paragraph { spans }
}

// ===========================================================================
// 收尾：去零宽、换表情
// ===========================================================================

fn finish(blocks: Vec<Block>, emotes: &[(String, String)]) -> Vec<Block> {
    blocks
        .into_iter()
        .filter_map(|block| match block {
            Block::Paragraph { spans } => {
                let spans = clean_spans(spans, emotes);
                if spans.is_empty() {
                    None
                } else {
                    Some(Block::Paragraph { spans })
                }
            }
            Block::Heading { spans } => {
                let spans = clean_spans(spans, emotes);
                if spans.is_empty() {
                    None
                } else {
                    Some(Block::Heading { spans })
                }
            }
            Block::Quote { spans } => {
                let spans = clean_spans(spans, emotes);
                if spans.is_empty() {
                    None
                } else {
                    Some(Block::Quote { spans })
                }
            }
            other => Some(other),
        })
        .collect()
}

fn clean_spans(spans: Vec<Span>, emotes: &[(String, String)]) -> Vec<Span> {
    let mut out: Vec<Span> = Vec::new();
    for span in spans {
        let Span::Text {
            text,
            bold,
            italic,
        } = &span
        else {
            out.push(span);
            continue;
        };
        let cleaned: String = text.chars().filter(|c| !is_zero_width(*c)).collect();
        if cleaned.is_empty() {
            continue;
        }
        if emotes.is_empty() {
            out.push(Span::Text {
                text: cleaned,
                bold: *bold,
                italic: *italic,
            });
            continue;
        }
        let mut last = 0usize;
        for (start, end, code) in find_emotes(&cleaned) {
            let Some((_, url)) = emotes.iter().find(|(c, _)| *c == code) else {
                continue;
            };
            if start > last {
                out.push(Span::Text {
                    text: cleaned[last..start].to_string(),
                    bold: *bold,
                    italic: *italic,
                });
            }
            out.push(Span::Emote {
                text: code.to_string(),
                url: url.clone(),
            });
            last = end;
        }
        if last < cleaned.len() {
            out.push(Span::Text {
                text: cleaned[last..].to_string(),
                bold: *bold,
                italic: *italic,
            });
        }
    }
    // 去掉只剩空白的文字段，避免一个段落变成空的
    if out
        .iter()
        .all(|s| matches!(s, Span::Text { text, .. } if is_blank(text)))
    {
        return Vec::new();
    }
    out
}

fn is_zero_width(c: char) -> bool {
    matches!(c, '\u{200b}' | '\u{200c}' | '\u{200d}' | '\u{2060}' | '\u{feff}')
}

/// Kotlin `EMOTE.matches(text)`：整串就是**一个**表情占位符（评论里的表情图片靠它认）。
fn is_emote(text: &str) -> bool {
    if !text.starts_with('[') || !text.ends_with(']') {
        return false;
    }
    matches!(find_emotes(text).next(), Some((0, end, _)) if end == text.len())
}

/// `\[[^\[\]\s<>]{1,10}]`：找出所有表情占位符（返回 `(起, 止, 代码)`，字节下标）。
fn find_emotes(text: &str) -> impl Iterator<Item = (usize, usize, &str)> {
    let mut found = Vec::new();
    let bytes = text.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] != b'[' {
            i += text[i..].chars().next().map(|c| c.len_utf8()).unwrap_or(1);
            continue;
        }
        // 从 '[' 后面开始数：1~10 个"不是 [] 空白 <> 的字符"，然后必须紧跟 ']'
        let mut j = i + 1;
        let mut count = 0usize;
        let mut end = None;
        while j < bytes.len() {
            let c = text[j..].chars().next().expect("下标在边界内");
            if c == ']' {
                if count >= 1 && count <= 10 {
                    end = Some(j);
                }
                break;
            }
            if c == '[' || is_java_whitespace(c) || c == '<' || c == '>' || count >= 10 {
                break;
            }
            count += 1;
            j += c.len_utf8();
        }
        match end {
            Some(e) => {
                found.push((i, e + 1, &text[i..e + 1]));
                i = e + 1;
            }
            None => i += 1,
        }
    }
    found.into_iter()
}

// ===========================================================================
// 地址
// ===========================================================================

/// jsoup `absUrl`：按 `https://www.zhihu.com/` 补全相对地址（属性不存在时是空串）。
/// jsoup `Element.absUrl(attr)`：**空的属性值也解析成 baseUri**（不是空串）。
///
/// 这一条实测过：`<img src>`（`src` 没有值）在 jsoup 里 `absUrl("src")` 是 `https://www.zhihu.com/`，
/// 所以 Kotlin 版会产出一个 `url = https://www.zhihu.com/` 的 `Block.Image`；Rust 原来返回空串再被
/// 后面的 `!value.trim().is_empty()` 丢掉，图片块就少了一个（差分测试里的"纯文本/简单标签"类）。
fn absolute_url(href: &str) -> String {
    if href.is_empty() {
        return BASE_URI.to_string();
    }
    if has_scheme(href) {
        return href.to_string();
    }
    if href.starts_with("//") {
        return format!("https:{href}");
    }
    if href.starts_with('/') {
        return format!("https://www.zhihu.com{href}");
    }
    if href.starts_with('#') || href.starts_with('?') {
        return format!("{BASE_URI}{href}");
    }
    format!("{BASE_URI}{href}")
}

fn has_scheme(url: &str) -> bool {
    let bytes = url.as_bytes();
    let Some(colon) = url.find(':') else {
        return false;
    };
    if colon == 0 || !bytes[0].is_ascii_alphabetic() {
        return false;
    }
    url[..colon]
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'-' | b'.'))
}

/// 够用的 URL 解析（OkHttp `toHttpUrlOrNull` 的子集）：只要 scheme / host / query。
pub mod url {
    /// 解析出来的绝对地址。
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Url {
        pub scheme: String,
        pub host: String,
        query: Vec<(String, String)>,
    }

    impl Url {
        /// OkHttp `queryParameter(name)`：名字精确匹配、取第一个，值解过百分号（`+` 算空格）。
        pub fn query_param(&self, name: &str) -> Option<String> {
            self.query
                .iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.clone())
        }
    }

    /// 解析 `http(s)://host/path?query#fragment`；不是 http(s) 绝对地址就 `None`（和 OkHttp 一样）。
    pub fn parse(url: &str) -> Option<Url> {
        let (scheme, rest) = url.split_once("://")?;
        let scheme = scheme.to_ascii_lowercase();
        if scheme != "http" && scheme != "https" {
            return None;
        }
        let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
        let authority = &rest[..authority_end];
        if authority.is_empty() {
            return None;
        }
        let host_port = authority.rsplit('@').next().unwrap_or(authority);
        let host = if let Some(stripped) = host_port.strip_prefix('[') {
            // IPv6
            stripped.split(']').next().unwrap_or("").to_ascii_lowercase()
        } else {
            host_port.split(':').next().unwrap_or("").to_ascii_lowercase()
        };
        if host.is_empty() {
            return None;
        }
        let mut query = Vec::new();
        if let Some(q_start) = rest.find('?') {
            let tail = &rest[q_start + 1..];
            let q = tail.split('#').next().unwrap_or("");
            for pair in q.split('&') {
                if pair.is_empty() {
                    continue;
                }
                let (name, value) = match pair.split_once('=') {
                    Some((n, v)) => (n, v),
                    None => (pair, ""),
                };
                query.push((percent_decode(name, true), percent_decode(value, true)));
            }
        }
        Some(Url {
            scheme,
            host,
            query,
        })
    }

    /// OkHttp 的 `percentDecode(plusIsSpace = true)`：`%XX` 解成字节，`+` 解成空格，坏转义原样留。
    pub fn percent_decode(input: &str, plus_is_space: bool) -> String {
        let bytes = input.as_bytes();
        let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
        let mut i = 0usize;
        while i < bytes.len() {
            match bytes[i] {
                b'%' if i + 2 < bytes.len() => {
                    match (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                        (Some(hi), Some(lo)) => {
                            out.push((hi << 4) | lo);
                            i += 3;
                        }
                        _ => {
                            out.push(b'%');
                            i += 1;
                        }
                    }
                }
                b'+' if plus_is_space => {
                    out.push(b' ');
                    i += 1;
                }
                b => {
                    out.push(b);
                    i += 1;
                }
            }
        }
        String::from_utf8_lossy(&out).into_owned()
    }

    fn hex(b: u8) -> Option<u8> {
        match b {
            b'0'..=b'9' => Some(b - b'0'),
            b'a'..=b'f' => Some(b - b'a' + 10),
            b'A'..=b'F' => Some(b - b'A' + 10),
            _ => None,
        }
    }

}

const BLOCK_TAGS: &[&str] = &[
    "p", "div", "section", "article", "header", "footer", "main", "aside", "nav", "li", "dl", "dt", "dd", "address",
    "details", "summary", "center",
];

/// `ownText()`：只算直接文字节点（jsoup 口径）。
impl Element {
    fn own_text(&self) -> String {
        let mut out = String::new();
        for node in &self.children {
            match node {
                crate::html::dom::Node::Text(text) => {
                    for c in text.chars() {
                        if matches!(c, ' ' | '\t' | '\n' | '\u{c}' | '\r' | '\u{a0}') {
                            if !out.ends_with(' ') {
                                out.push(' ');
                            }
                        } else if !matches!(c, '\u{ad}' | '\u{200b}') {
                            out.push(c);
                        }
                    }
                }
                crate::html::dom::Node::Element(child) => {
                    if child.name == "br" && !out.ends_with(' ') {
                        out.push(' ');
                    }
                }
            }
        }
        out.trim_matches(|c: char| (c as u32) <= 0x20).to_string()
    }

    /// `selectFirst("span.title")`：第一个 `span` 且 class 里有 `title`。
    fn select_first_class(&self, tag: &str, class: &str) -> Option<&Element> {
        for child in self.child_elements() {
            if child.name == tag && child.has_class(class) {
                return Some(child);
            }
            if let Some(found) = child.select_first_class(tag, class) {
                return Some(found);
            }
        }
        None
    }
}

