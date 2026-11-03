//! **HTML5 分词器 + "in body" 树构造的一个够用子集**（jsoup 1.18.3 的替身）。
//!
//! 全仓**只有这一份** HTML 树构造：
//! 知乎正文 / 评论（[`crate::zhihu::html`]）和小黑盒正文 / 评论（[`crate::html::convert`] /
//! `heybox::detail`）都吃这棵树。
//!
//! 为什么要自己写：Kotlin 版走的是 jsoup（`Jsoup.parseBodyFragment`），Rust 这边离线构建、
//! cargo 缓存里没有任何 HTML 解析 crate（也没有 XML 的），而**正文是用户能看见的东西**，
//! 不能"大概齐"。这一层只覆盖编辑器真的会产出的那套标签（`p` / `figure` / `img` / `a` /
//! `b` / `i` / `ul` / `li` / `blockquote` / `h1~h6` / `sup` / `br` / `hr` / `span` / `noscript` …），
//! 口径逐条对着 jsoup 1.18.3 实测（探针结论写在下面各条注释里）。
//!
//! 和 jsoup 的差别（都是**畸形 HTML** 才看得出来，真实正文不会出现）：
//! - **不实现"重建活跃格式化元素"**（HTML5 的 adoption agency）：`<b>x<p>y</p>` 里 jsoup 会在 `p`
//!   里重建一个 `<b>`，这里就让 `b` 一直开着（语义上都是"y 也是粗体"，只是 `<b>` 的位置不同）；
//! - `<a>` 跨块没闭合时会少一个重建的链接（同上一条的同一个原因）；
//! - `<table>` 只做"补 `tbody` / 单元格隐式闭合"，不做 foster parenting（表格里的非表格内容）；
//! - **树最多 255 层**（[`MAX_DEPTH`]）：更深的嵌套被摊平成兄弟节点，文本不丢、容器层级丢。
//!   真实正文最大 9 层；只有刻意构造的畸形输入才碰得到，碰到的差异由差分测试按模式记成已知差异。
//!
//! 实体解码**只解一遍**（HTML 分词器的口径）：`&amp;lt;` → `&lt;`，不是 `<`。这和 B站 代码块
//! 那种"解到不再变化"不是一回事（见 [`crate::html::unescape_entities_looped`]）。

use crate::bili::entities;

/// DOM 节点。
#[derive(Debug, Clone, PartialEq)]
pub enum Node {
    Text(String),
    Element(Element),
}

/// 一个元素。属性名统一小写、值已经解过实体（和 jsoup 一样）。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Element {
    pub name: String,
    pub attrs: Vec<(String, String)>,
    pub children: Vec<Node>,
}

/// 树的最大深度（**含根**，也就是最多 255 层元素嵌套）。
///
/// 为什么要设上限：树一旦可以任意深，
/// 1. 递归的 drop glue / `Clone` / 递归访问都会把栈撑爆（桌面测试线程可以给 256 KB 的小栈）；
/// 2. `find_in_scope` 每次开始标签都要从栈顶往下扫，深度 d 的输入会退化到 O(d²)（5000 层要几秒）。
///
/// 真实知乎正文（仓库里 10 份详情 + 8 份评论 fixture）最大深度是 **9**，256 是它的二十多倍。
/// 超过上限的部分**仍然保留文本和元素**，只是挂成当前父元素的兄弟（不再往下开新层）——可见文字
/// 一个字节都不丢，丢的只有畸形容器的嵌套本身（本文件顶部"和 jsoup 的差别"里有说明）。
const MAX_DEPTH: usize = 256;

impl Drop for Element {
    /// 深树**迭代**释放：默认的 drop glue 是递归的，几万层嵌套在 256 KB 的栈上会直接段错误
    /// （整个进程没了，`catch_unwind` 也接不住）。这里把子树拆到一个显式的栈上逐个丢，
    /// 每个 `Element` 真正执行 drop 时 children 已经被搬空，所以不会有递归。
    fn drop(&mut self) {
        let mut pending = std::mem::take(&mut self.children);
        while let Some(node) = pending.pop() {
            if let Node::Element(mut child) = node {
                pending.append(&mut child.children);
            }
        }
    }
}

impl Element {
    fn new(name: &str) -> Self {
        Element {
            name: name.to_string(),
            attrs: Vec::new(),
            children: Vec::new(),
        }
    }

    /// jsoup `attr(name)`：没有就是空串；同名属性取**第一个**。
    pub fn attr(&self, name: &str) -> &str {
        self.attrs
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
            .unwrap_or("")
    }

    pub fn has_attr(&self, name: &str) -> bool {
        self.attrs.iter().any(|(k, _)| k == name)
    }

    pub fn attr_trimmed(&self, name: &str) -> &str {
        self.attr(name).trim()
    }

    /// 小写标签名（解析时就转了；jsoup 的 `normalName()`）。
    pub fn name(&self) -> &str {
        &self.name
    }

    /// 同 [`Element::name`]，用 jsoup 那边的名字叫（`html::convert` 的四个钩子按这个写）。
    pub fn normal_name(&self) -> &str {
        &self.name
    }

    /// jsoup `classNames()`：`class` 属性按空白切开。
    pub fn class_names(&self) -> Vec<&str> {
        self.attr("class")
            .split(|c: char| matches!(c, ' ' | '\t' | '\n' | '\r' | '\u{c}'))
            .filter(|s| !s.is_empty())
            .collect()
    }

    pub fn has_class(&self, class: &str) -> bool {
        self.class_names().contains(&class)
    }

    /// jsoup `className()`：`class` 属性的值**两头 trim 过**（`String.trim()` 的口径，`<= ' '`）。
    pub fn class_name(&self) -> &str {
        self.attr("class").trim_matches(|c: char| (c as u32) <= 0x20)
    }

    /// Kotlin `String.toIntOrNull()`：可选正负号 + ASCII 数字，且要落在 i32 里。
    pub fn attr_int(&self, name: &str) -> Option<i32> {
        to_int_or_null(self.attr(name))
    }

    /// jsoup `absUrl(attr)`：按 `base` 补全（口径逐条见 [`resolve_url`]）。
    /// **属性不存在**时是空串；**属性在、值是空串**时是 `base` 本身。
    pub fn abs_url(&self, base: &str, attr: &str) -> String {
        resolve_url(base, self.attr(attr))
    }

    /// 直接子元素（jsoup `children()`）。
    pub fn child_elements(&self) -> impl Iterator<Item = &Element> {
        self.children.iter().filter_map(|n| match n {
            Node::Element(e) => Some(e),
            Node::Text(_) => None,
        })
    }

    /// 节点的孩子（jsoup `childNodes()`）。**顺序就是文档顺序**。
    pub fn child_nodes(&self) -> &[Node] {
        &self.children
    }

    /// 深度优先的第一个同名后代（jsoup `selectFirst("img")` 之类）。
    ///
    /// **迭代**实现（显式栈）：树可以很深，递归访问会在小栈上把进程带走。
    pub fn select_first(&self, name: &str) -> Option<&Element> {
        let mut stack: Vec<std::slice::Iter<'_, Node>> = vec![self.children.iter()];
        while let Some(iter) = stack.pop() {
            let mut iter = iter;
            while let Some(node) = iter.next() {
                if let Node::Element(child) = node {
                    if child.name == name {
                        return Some(child);
                    }
                    stack.push(iter);
                    stack.push(child.children.iter());
                    break;
                }
            }
        }
        None
    }

    /// 全部同名后代（文档顺序）。
    pub fn select_all<'a>(&'a self, name: &str, out: &mut Vec<&'a Element>) {
        let mut stack: Vec<std::slice::Iter<'_, Node>> = vec![self.children.iter()];
        while let Some(mut iter) = stack.pop() {
            while let Some(node) = iter.next() {
                if let Node::Element(child) = node {
                    if child.name == name {
                        out.push(child);
                    }
                    stack.push(iter);
                    stack.push(child.children.iter());
                    break;
                }
            }
        }
    }

    /// 全部同名后代（文档顺序的 `Vec`；jsoup `select(tag)` 的够用版）。
    pub fn select(&self, name: &str) -> Vec<&Element> {
        let mut out = Vec::new();
        self.select_all(name, &mut out);
        out
    }

    /// 所有后代（不含自己），文档顺序。
    pub fn descendants(&self) -> Vec<&Element> {
        let mut out = Vec::new();
        let mut stack: Vec<std::slice::Iter<'_, Node>> = vec![self.children.iter()];
        while let Some(mut iter) = stack.pop() {
            while let Some(node) = iter.next() {
                if let Node::Element(child) = node {
                    out.push(child);
                    stack.push(iter);
                    stack.push(child.children.iter());
                    break;
                }
            }
        }
        out
    }

    /// jsoup `Element.text()`：空白压成一个空格（**NBSP 也算空白**），块级元素之间补一个空格，
    /// 丢掉 U+00AD / U+200B 这两个"看不见的字符"。最后按 Java 的 `String.trim()`（`<= ' '`）去两头。
    ///
    /// **迭代**实现，而且"开头全是空格"用布尔量跟着走——原来每个文本节点都扫一遍已经攒出来的
    /// 字符串（`out.chars().all(...)`），上万个文本节点时会退化成 O(n²)。
    pub fn text(&self) -> String {
        enum Frame<'a> {
            Children(std::slice::Iter<'a, Node>),
            AfterBlockSpace,
        }
        let mut out = String::new();
        let mut only_spaces = true;
        let mut stack: Vec<Frame<'_>> = vec![Frame::Children(self.children.iter())];
        while let Some(frame) = stack.pop() {
            match frame {
                Frame::AfterBlockSpace => {
                    if !out.is_empty() && !out.ends_with(' ') {
                        out.push(' ');
                    }
                }
                Frame::Children(mut iter) => match iter.next() {
                    None => {}
                    Some(node) => {
                        stack.push(Frame::Children(iter));
                        match node {
                            Node::Text(t) => append_normalised_whitespace(&mut out, t, &mut only_spaces),
                            Node::Element(child) => {
                                // jsoup：块级元素（和 br）**前后**各补一个空格
                                let block = is_block_tag(&child.name);
                                let br = child.name == "br";
                                if !out.is_empty() && (block || br) && !out.ends_with(' ') {
                                    out.push(' ');
                                }
                                if !br {
                                    if block {
                                        stack.push(Frame::AfterBlockSpace);
                                    }
                                    stack.push(Frame::Children(child.children.iter()));
                                }
                            }
                        }
                    }
                },
            }
        }
        // Java `String.trim()`：去掉两端 <= ' ' 的字符
        out.trim_matches(|c: char| (c as u32) <= 0x20).to_string()
    }

    /// jsoup `Element.wholeText()`：原样拼文本节点，`<br>` 记一个 `\n`（块级元素本身不加分隔）。
    pub fn whole_text(&self) -> String {
        let mut out = String::new();
        let mut stack: Vec<std::slice::Iter<'_, Node>> = vec![self.children.iter()];
        while let Some(mut iter) = stack.pop() {
            while let Some(node) = iter.next() {
                match node {
                    Node::Text(t) => out.push_str(t),
                    Node::Element(child) => {
                        if child.name == "br" {
                            out.push('\n');
                        } else {
                            stack.push(iter);
                            stack.push(child.children.iter());
                            break;
                        }
                    }
                }
            }
        }
        out
    }
}

/// Kotlin `String.toIntOrNull()` 的口径（可选 `+`/`-`、只认 ASCII 数字、要放进 i32）。
pub fn to_int_or_null(s: &str) -> Option<i32> {
    let bytes = s.as_bytes();
    if bytes.is_empty() {
        return None;
    }
    let (negative, digits) = match bytes[0] {
        b'+' => (false, &bytes[1..]),
        b'-' => (true, &bytes[1..]),
        _ => (false, bytes),
    };
    if digits.is_empty() || !digits.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let text = std::str::from_utf8(digits).ok()?;
    let value: i64 = text.parse().ok()?;
    let value = if negative { -value } else { value };
    i32::try_from(value).ok()
}

/// HTML5 的块级元素。
fn is_block_tag(name: &str) -> bool {
    matches!(
        name,
        "html"
            | "head"
            | "body"
            | "frameset"
            | "script"
            | "noscript"
            | "style"
            | "meta"
            | "link"
            | "title"
            | "frame"
            | "noframes"
            | "section"
            | "nav"
            | "aside"
            | "hgroup"
            | "header"
            | "footer"
            | "p"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "ul"
            | "ol"
            | "pre"
            | "div"
            | "blockquote"
            | "hr"
            | "address"
            | "figure"
            | "figcaption"
            | "form"
            | "fieldset"
            | "ins"
            | "del"
            | "dl"
            | "dt"
            | "dd"
            | "li"
            | "table"
            | "caption"
            | "thead"
            | "tfoot"
            | "tbody"
            | "colgroup"
            | "col"
            | "tr"
            | "th"
            | "td"
            | "video"
            | "audio"
            | "canvas"
            | "details"
            | "menu"
            | "plaintext"
            | "listing"
            | "template"
            | "article"
            | "main"
            | "svg"
            | "math"
            | "center"
            | "marquee"
            | "summary"
            | "dialog"
            | "dir"
            | "applet"
            | "iframe"
            | "object"
            | "button"
    )
}

/// jsoup `Element.absUrl(attr)` / `StringUtil.resolve(base, rel)` 的够用实现。
///
/// jsoup 1.18.3 实测（探针 `Probe7` / `Probe14`）：
/// - 相对地址拼在 base 的**目录**后面：`https://a.com/x/y` + `r` → `https://a.com/x/r`；
/// - `./`、`../`、`../../..` 会做点段归一（打到根就停）；
/// - `/p` 是根路径、`//h/p` 只换 host、`?q` / `#f` 顶掉 base 的 query / fragment；
/// - **值是空串**时返回 base 本身（连 query / fragment 一起保留）；
/// - `http:` / `mailto:` / `javascript:` / `heybox:` / `data:` 这类**有 scheme 的原样**（大小写也原样）；
/// - **base 自己不是合法绝对地址**（空串 / `bad base`）时返回空串。
pub fn resolve_url(base: &str, rel: &str) -> String {
    // 有 scheme 的相对地址：原样返回（jsoup 连大小写都不动）
    if let Some(colon) = rel.find(':') {
        let scheme = &rel[..colon];
        if !scheme.is_empty()
            && scheme
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
            && scheme.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
        {
            return rel.to_string();
        }
    }
    let base = base.trim();
    let Some((scheme, rest)) = base.split_once("://") else {
        return String::new();
    };
    if scheme.is_empty() {
        return String::new();
    }
    // 权威段（host[:port]）到第一个 `/`、`?`、`#` 为止
    let auth_end = rest
        .find(|c: char| matches!(c, '/' | '?' | '#'))
        .unwrap_or(rest.len());
    let authority = &rest[..auth_end];
    let tail = &rest[auth_end..];
    // base 的 path / query / fragment
    let (base_path, base_query) = match tail.find(['?', '#']) {
        Some(i) => (&tail[..i], &tail[i..]),
        None => (tail, ""),
    };
    let base_path = if base_path.is_empty() { "/" } else { base_path };

    if rel.is_empty() {
        return format!("{scheme}://{authority}{base_path}{base_query}");
    }
    if rel.starts_with("//") {
        return format!("{scheme}:{rel}");
    }
    if rel.starts_with('/') {
        return format!("{scheme}://{authority}{}", normalize_dots(rel));
    }
    if let Some(frag) = rel.strip_prefix('#') {
        // `#g`：保留 base 的 path + query
        return format!("{scheme}://{authority}{base_path}{base_query}#{frag}");
    }
    if let Some(query) = rel.strip_prefix('?') {
        return format!("{scheme}://{authority}{base_path}?{query}");
    }
    // 相对路径：base 的目录 + rel，再做点段归一
    let dir = match base_path.rfind('/') {
        Some(i) => &base_path[..=i],
        None => "/",
    };
    format!("{scheme}://{authority}{}", normalize_dots(&format!("{dir}{rel}")))
}

/// 点段归一（`RFC 3986 §5.2.4` 的够用版）：`a/b/../c` → `a/c`、`./x` → `x`，打到根就停。
fn normalize_dots(path: &str) -> String {
    let leading = path.starts_with('/');
    let mut out: Vec<&str> = Vec::new();
    let segments: Vec<&str> = path.split('/').collect();
    let last = segments.len().saturating_sub(1);
    for (i, seg) in segments.iter().enumerate() {
        match *seg {
            "." => {
                if i == last {
                    out.push("");
                }
            }
            ".." => {
                if out.len() > 1 || (out.len() == 1 && !out[0].is_empty()) {
                    out.pop();
                }
                if i == last {
                    out.push("");
                }
            }
            other => out.push(other),
        }
    }
    let mut joined = out.join("/");
    if leading && !joined.starts_with('/') {
        joined.insert(0, '/');
    }
    if joined.is_empty() {
        "/".to_string()
    } else {
        joined
    }
}

/// 自己实现的 jsoup `StringUtil.appendNormalisedWhitespace`：
/// 空白（` ` `\t` `\n` `\f` `\r` 和 **NBSP**）压成一个空格，U+00AD / U+200B 直接丢掉，
/// 别的一律原样。
///
/// `only_spaces` 是"`out` 到目前为止全是空格"（空串也算）——等价于 jsoup 的
/// `out.chars().all(|c| c == ' ')`，但不用每次重新扫一遍（见 [`Element::text`]）。
fn append_normalised_whitespace(out: &mut String, text: &str, only_spaces: &mut bool) {
    let strip_leading = *only_spaces;
    let mut last_was_white = false;
    let mut reached_non_white = false;
    for c in text.chars() {
        if is_actually_whitespace(c) {
            if (strip_leading && !reached_non_white) || last_was_white {
                continue;
            }
            out.push(' ');
            last_was_white = true;
        } else if is_invisible(c) {
            continue;
        } else {
            out.push(c);
            last_was_white = false;
            reached_non_white = true;
            *only_spaces = false;
        }
    }
}

fn is_actually_whitespace(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\u{c}' | '\r' | '\u{a0}')
}

fn is_invisible(c: char) -> bool {
    matches!(c, '\u{ad}' | '\u{200b}')
}

// ===========================================================================
// 分词
// ===========================================================================

/// 从 `<` 起是哪种东西。
enum Markup {
    Comment,
    /// `<!DOCTYPE>` / `<![CDATA[]]>` / `<?...>` 之类：整段丢掉（CDATA 的内容当文字，和 jsoup 一样）。
    Declaration,
    CData,
    EndTag,
    StartTag,
    /// `<` 后面不是字母：就是一个普通文字 `<`。
    Text,
}

/// 树构造器：一个"当前路径"栈，栈顶是正在填的父元素。
struct TreeBuilder {
    stack: Vec<Element>,
}

impl TreeBuilder {
    fn new() -> Self {
        TreeBuilder {
            stack: vec![Element::new("#root")],
        }
    }

    fn finish(mut self) -> Element {
        // 没闭合的元素在片段结束时一并关掉（HTML5 的做法）
        self.pop_to(1);
        debug_assert_eq!(self.stack.len(), 1);
        self.stack.pop().expect("栈里至少有根")
    }

    fn current(&mut self) -> &mut Element {
        self.stack.last_mut().expect("栈里至少有根")
    }

    fn append_text(&mut self, mut text: String) {
        if text.is_empty() {
            return;
        }
        let parent = self.current();
        // `pre` / `listing` 的**第一个字符是换行时丢掉那个换行**（HTML5 的 "ignore the first LF"，
        // jsoup 也这么做：`<pre>\nX</pre>` 的 wholeText 是 `X`，`<pre>\n\nX</pre>` 是 `\nX`）。
        // 只对**直接挂在 `pre` / `listing` 上的第一个文本节点**生效：`<pre><code>\nX</code></pre>`
        // 里那个换行留着。
        if matches!(parent.name.as_str(), "pre" | "listing")
            && parent.children.is_empty()
            && text.starts_with('\n')
        {
            text.remove(0);
            if text.is_empty() {
                return;
            }
        }
        if let Some(Node::Text(existing)) = parent.children.last_mut() {
            existing.push_str(&text);
            return;
        }
        parent.children.push(Node::Text(text));
    }

    fn append_element(&mut self, el: Element) {
        self.current().children.push(Node::Element(el));
    }

    /// 把元素压成"正在填的父元素"；**超过 [`MAX_DEPTH`] 就只挂成兄弟、不再往下开层**。
    /// 这样深树既不会撑爆递归，也不会让 [`TreeBuilder::find_in_scope`] 的栈扫描退化。
    fn push_open(&mut self, el: Element) {
        if self.stack.len() >= MAX_DEPTH {
            self.append_element(el);
        } else {
            self.stack.push(el);
        }
    }

    /// 栈里从顶往下找第一个 `name`；碰到 `boundaries` 里的标签就停（HTML5 的 "in scope"）。
    fn find_in_scope(&self, name: &str, boundaries: &[&str]) -> Option<usize> {
        for (index, el) in self.stack.iter().enumerate().rev() {
            if el.name == name {
                return Some(index);
            }
            if index == 0 || boundaries.contains(&el.name.as_str()) {
                return None;
            }
        }
        None
    }

    /// 把栈截到 `index`（不含）：等于"关掉 `index` 这个元素和它上面的一切"。
    /// 关的时候要把元素**挂回父元素**（栈里是所有权，直接 `truncate` 会把子树丢掉）。
    fn pop_to(&mut self, index: usize) {
        while self.stack.len() > index {
            let el = self.stack.pop().expect("栈非空");
            if let Some(parent) = self.stack.last_mut() {
                parent.children.push(Node::Element(el));
            }
        }
    }

    fn run(&mut self, html: &str) {
        let bytes = html.as_bytes();
        let mut i = 0usize;
        while i < bytes.len() {
            if bytes[i] != b'<' {
                let end = html[i..].find('<').map(|rel| i + rel).unwrap_or(bytes.len());
                let text = decode_entities(&normalize_newlines(&html[i..end]), false);
                self.append_text(text);
                i = end;
                continue;
            }
            match classify(html, i) {
                Markup::Text => {
                    self.append_text("<".to_string());
                    i += 1;
                }
                Markup::Comment => {
                    i = skip_comment(html, i);
                }
                Markup::Declaration => {
                    i = skip_to_gt(html, i);
                }
                Markup::CData => {
                    let start = i + "<![CDATA[".len();
                    let end = html[start..].find("]]>").map(|rel| start + rel).unwrap_or(bytes.len());
                    self.append_text(normalize_newlines(&html[start..end]));
                    i = if end < bytes.len() { end + 3 } else { bytes.len() };
                }
                Markup::EndTag => {
                    let (name, next) = parse_end_tag(html, i);
                    self.end_tag(&name);
                    i = next;
                }
                Markup::StartTag => {
                    let (name, attrs, self_closing, terminated, next) = parse_start_tag(html, i);
                    if !terminated {
                        // 输入末尾的半截标签：HTML5 在 EOF 处整段丢掉（jsoup 也是）。
                        // 注意 **raw text 元素也一样**：`<textarea><b` 在 jsoup 里连 `<textarea>` 都不建。
                        break;
                    }
                    if is_raw_text(&name) {
                        let (content, after) = read_raw_text(html, next, &name);
                        // jsoup 的 script / style 内容在 DataNode 里：Element.text() / wholeText() 都看不见它
                        let text = if matches!(name.as_str(), "script" | "style") {
                            String::new()
                        } else if is_rcdata(&name) {
                            decode_entities(&normalize_newlines(&content), false)
                        } else {
                            normalize_newlines(&content)
                        };
                        let mut el = Element {
                            name: name.clone(),
                            attrs,
                            children: Vec::new(),
                        };
                        if !text.is_empty() {
                            el.children.push(Node::Text(text));
                        }
                        self.append_element(el);
                        i = after;
                        continue;
                    }
                    self.start_tag(&name, attrs, self_closing);
                    i = next;
                }
            }
        }
    }

    fn start_tag(&mut self, name: &str, attrs: Vec<(String, String)>, self_closing: bool) {
        // `<image>` 是 `<img>` 的老名字（HTML5 分词器直接换名）。jsoup 实测：`<image src=a>` 就是
        // `<img src="a">`。
        let name = if name == "image" { "img" } else { name };
        // 外壳标签（`<html>` / `<body>` / `<head>`）**不建节点、只留内容**：
        // jsoup 的 `parseBodyFragment` 里它们进不了 `body()`（`<body><p>x</p></body>` 的 html 就是
        // `<p>x</p>`）。原来这里会多出一层节点，`<head><title>t</title></head>x` 的 title 也就挂错了地方。
        if is_shell_tag(name) {
            return;
        }
        // 表格结构标签**没有开着的 `<table>` 时整段丢掉**（HTML5 "in body"：这些 token 在
        // 表格外是"解析错误、忽略标签"，内容照样留在当前元素里）。jsoup 实测：
        // `<tbody><p>x</p></tbody>` 的 body 就是 `<p>x</p>`，`<td>1</td>` 的 body 就是文字 `1`。
        // 原来这里会把 `tbody` / `td` 当普通元素挂上去，多出一层容器（差分测试里"其它"那一类）。
        if is_table_structure(name) && self.find_in_scope("table", &["#root"]).is_none() {
            return;
        }
        let ends_p = closes_paragraph(name);
        if ends_p {
            self.close_paragraph();
        }
        // 别的隐式结束（HTML5 "in body" 的那几条）
        match name {
            "li" => self.close_in_scope("li", LIST_ITEM_BOUNDARIES),
            "dd" | "dt" => {
                if self.find_in_scope("dd", SCOPE_BOUNDARIES).is_some() {
                    self.close_in_scope("dd", SCOPE_BOUNDARIES);
                } else {
                    self.close_in_scope("dt", SCOPE_BOUNDARIES);
                }
            }
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                if is_heading(&self.stack.last().expect("栈里至少有根").name) {
                    self.close_top();
                }
            }
            "option" => {
                if self.stack.last().expect("栈里至少有根").name == "option" {
                    self.close_top();
                }
            }
            "optgroup" => {
                if self.stack.last().expect("栈里至少有根").name == "option" {
                    self.close_top();
                }
                if self.stack.last().expect("栈里至少有根").name == "optgroup" {
                    self.close_top();
                }
            }
            "a" => self.close_in_scope("a", SCOPE_BOUNDARIES),
            "button" => self.close_in_scope("button", SCOPE_BOUNDARIES),
            "tbody" | "thead" | "tfoot" => {
                for cell in ["td", "th"] {
                    self.close_in_scope(cell, TABLE_SECTION_BOUNDARIES);
                }
                self.close_in_scope("tr", TABLE_SECTION_BOUNDARIES);
                self.close_in_scope("tbody", TABLE_SECTION_BOUNDARIES);
                self.close_in_scope("thead", TABLE_SECTION_BOUNDARIES);
                self.close_in_scope("tfoot", TABLE_SECTION_BOUNDARIES);
            }
            "tr" => {
                for cell in ["td", "th"] {
                    self.close_in_scope(cell, TABLE_SECTION_BOUNDARIES);
                }
                self.close_in_scope("tr", TABLE_SECTION_BOUNDARIES);
                if self.find_in_scope("table", &["#root"]).is_some()
                    && !matches!(
                        self.stack.last().expect("栈里至少有根").name.as_str(),
                        "tbody" | "thead" | "tfoot"
                    )
                {
                    let tbody = Element::new("tbody");
                    self.push_open(tbody);
                }
            }
            "td" | "th" => {
                for cell in ["td", "th"] {
                    self.close_in_scope(cell, TABLE_SECTION_BOUNDARIES);
                }
            }
            _ => {}
        }
        let el = Element {
            name: name.to_string(),
            attrs,
            children: Vec::new(),
        };
        // 自闭合的标签**不压栈**（jsoup 对已知标签也是这么处理的：`<div/>x` 里 x 在 div 外面）
        if is_void(name) || self_closing {
            self.append_element(el);
            return;
        }
        self.push_open(el);
    }

    /// 关掉当前打开的元素（把它连同子树挂回父元素）。
    ///
    /// 不能直接 `self.stack.pop()`：栈里是**所有权**，pop 出来的元素不会回到父元素的 children 里，
    /// 它下面已经攒好的内容就悄悄没了（`<h1>a<h2>b` 这种连续标题会丢 "a"）。
    fn close_top(&mut self) {
        let index = self.stack.len().saturating_sub(1);
        self.pop_to(index);
    }

    fn end_tag(&mut self, name: &str) {
        // `</h1>`..`</h6>` 关掉**任意**开着的标题（HTML5 "in body"：`</h2>` 也能关掉 `<h1>`）。
        // jsoup 实测：`<h1>a</h2>b` 的 body 是 `<h1>a</h1>b`。
        if is_heading(name) {
            for level in ["h1", "h2", "h3", "h4", "h5", "h6"] {
                if let Some(index) = self.find_in_scope(level, &["#root"]) {
                    self.pop_to(index);
                    return;
                }
            }
            return;
        }
        if is_void(name) {
            // `</br>` 在 HTML5 里**等于 `<br>`**。
            // 别的空元素结束标签直接丢掉。
            if name == "br" {
                self.append_element(Element::new("br"));
            }
            return;
        }
        if name == "p" {
            if !self.close_paragraph() {
                // 没有开着的 p：补一个空的再关掉（HTML5 的规定，jsoup 也一样）
                let el = Element::new("p");
                self.append_element(el);
            }
            return;
        }
        if let Some(index) = self.find_in_scope(name, &["#root"]) {
            self.pop_to(index);
        }
    }

    fn close_paragraph(&mut self) -> bool {
        match self.find_in_scope("p", BUTTON_SCOPE_BOUNDARIES) {
            Some(index) => {
                self.pop_to(index);
                true
            }
            None => false,
        }
    }

    fn close_in_scope(&mut self, name: &str, boundaries: &[&str]) {
        if let Some(index) = self.find_in_scope(name, boundaries) {
            self.pop_to(index);
        }
    }
}

const SCOPE_BOUNDARIES: &[&str] = &["applet", "caption", "html", "table", "td", "th", "marquee", "object", "template"];
const BUTTON_SCOPE_BOUNDARIES: &[&str] = &[
    "applet", "caption", "html", "table", "td", "th", "marquee", "object", "template", "button",
];
const LIST_ITEM_BOUNDARIES: &[&str] = &[
    "applet", "caption", "html", "table", "td", "th", "marquee", "object", "template", "ol", "ul",
];
const TABLE_SECTION_BOUNDARIES: &[&str] = &["html", "table", "template"];

fn is_heading(name: &str) -> bool {
    matches!(name, "h1" | "h2" | "h3" | "h4" | "h5" | "h6")
}

/// 这些开始标签会先关掉开着的 `<p>`（HTML5 "in body" 那张表）。
fn closes_paragraph(name: &str) -> bool {
    is_heading(name)
        || matches!(
            name,
            "address"
                | "article"
                | "aside"
                | "blockquote"
                | "center"
                | "details"
                | "dialog"
                | "dir"
                | "div"
                | "dl"
                | "fieldset"
                | "figcaption"
                | "figure"
                | "footer"
                | "header"
                | "hgroup"
                | "main"
                | "menu"
                | "nav"
                | "ol"
                | "p"
                | "section"
                | "summary"
                | "ul"
                | "pre"
                | "listing"
                | "form"
                | "table"
                | "hr"
                | "xmp"
                | "plaintext"
        )
}

fn is_void(name: &str) -> bool {
    matches!(
        name,
        "area"
            | "base"
            | "basefont"
            | "bgsound"
            | "link"
            | "meta"
            | "br"
            | "img"
            | "embed"
            | "param"
            | "hr"
            | "input"
            | "keygen"
            | "source"
            | "track"
            | "wbr"
            | "col"
            | "frame"
    )
}

/// 表格结构标签：表格**外**出现时整段丢掉（HTML5 "in body" 的忽略规则）。
fn is_table_structure(name: &str) -> bool {
    matches!(
        name,
        "caption" | "col" | "colgroup" | "tbody" | "thead" | "tfoot" | "tr" | "td" | "th"
    )
}

/// 片段里的外壳标签：jsoup 的 `parseBodyFragment` 不把它们放进 `body()`（只留内容）。
fn is_shell_tag(name: &str) -> bool {
    matches!(name, "html" | "body" | "head" | "frameset")
}

/// 内容**不当标签解析**的元素（HTML5 的 raw text + RCDATA）。
///
/// `iframe` / `noembed` / `noframes` / `xmp` 是 raw text（内容原样当文字、实体**不解**），
/// `textarea` / `title` 是 RCDATA（内容当文字、实体照解）。`listing` **不在这里**：jsoup 会照常
/// 解析它里面的标签。
fn is_raw_text(name: &str) -> bool {
    matches!(
        name,
        "script"
            | "style"
            | "textarea"
            | "title"
            | "xmp"
            | "noembed"
            | "noframes"
            | "iframe"
            | "plaintext"
    )
}

/// RCDATA：内容里的实体照样解（`textarea` / `title`）。
fn is_rcdata(name: &str) -> bool {
    matches!(name, "textarea" | "title")
}

fn classify(html: &str, i: usize) -> Markup {
    let rest = &html[i..];
    if rest.starts_with("<!--") {
        return Markup::Comment;
    }
    if rest.starts_with("<![CDATA[") {
        return Markup::CData;
    }
    if rest.starts_with("<!") || rest.starts_with("<?") {
        return Markup::Declaration;
    }
    if let Some(after) = rest.strip_prefix("</") {
        let mut chars = after.chars();
        return match chars.next() {
            Some(c) if c.is_ascii_alphabetic() => Markup::EndTag,
            // 孤立的 `</`：HTML5 的分词器当**文本**（jsoup：文本就是 "</"）。原来当声明整段跳过，文字就丢了
            // （差分测试里"其它"的一类）。`</>` 是解析错误、什么都不输出：当声明跳过这 3 个字节。
            Some('>') => Markup::Declaration,
            None => Markup::Text,
            _ => Markup::Declaration,
        };
    }
    match rest[1..].chars().next() {
        Some(c) if c.is_ascii_alphabetic() => Markup::StartTag,
        _ => Markup::Text,
    }
}

fn skip_comment(html: &str, i: usize) -> usize {
    let start = i + "<!--".len();
    match html[start..].find("-->") {
        Some(rel) => start + rel + 3,
        None => html.len(),
    }
}

fn skip_to_gt(html: &str, i: usize) -> usize {
    match html[i..].find('>') {
        Some(rel) => i + rel + 1,
        None => html.len(),
    }
}

fn is_tag_name_char(b: u8) -> bool {
    !matches!(b, b' ' | b'\t' | b'\n' | b'\r' | 0x0c | b'/' | b'>')
}

/// 结束标签：`</name ...>`。name 之后的垃圾一律忽略（jsoup 也不报错）。
fn parse_end_tag(html: &str, i: usize) -> (String, usize) {
    let bytes = html.as_bytes();
    let mut j = i + 2;
    let start = j;
    while j < bytes.len() && is_tag_name_char(bytes[j]) {
        j += 1;
    }
    let name = html[start..j].to_ascii_lowercase();
    while j < bytes.len() && bytes[j] != b'>' {
        j += 1;
    }
    (name, if j < bytes.len() { j + 1 } else { bytes.len() })
}

/// 开始标签：`<name attr=value ...>`，返回 `(name, attrs, self_closing, 下一个字节)`。
/// 读开始标签。返回值最后多一个是**有没有遇到 `>`**：没有就是"被输入末尾截断的半截标签"，
/// HTML5 的分词器在 EOF 处直接把它**丢掉**（jsoup：`"<img src"` 的 body 是空、`"<p><img src"` 只有 `<p></p>`），
/// 原来这里照样当成一个完整标签建了节点（差分测试里"插入怪字符 / 纯随机"那一类）。
fn parse_start_tag(html: &str, i: usize) -> (String, Vec<(String, String)>, bool, bool, usize) {
    let bytes = html.as_bytes();
    let mut j = i + 1;
    let start = j;
    while j < bytes.len() && is_tag_name_char(bytes[j]) {
        j += 1;
    }
    let name = html[start..j].to_ascii_lowercase();
    let mut attrs: Vec<(String, String)> = Vec::new();
    // 同名属性**取第一个**。原来这里写的是"后来的覆盖先来的"，反了。
    // 少量属性线性扫就够；属性特别多（几十个以上，只有畸形输入才可能）时补一张下标表，免得 O(属性数²)。
    let mut attr_index: Option<std::collections::HashMap<String, usize>> = None;
    let mut self_closing = false;
    let mut terminated = false;
    while j < bytes.len() {
        // 跳过空白
        while j < bytes.len() && is_html_space(bytes[j]) {
            j += 1;
        }
        if j >= bytes.len() {
            break;
        }
        match bytes[j] {
            b'>' => {
                j += 1;
                terminated = true;
                break;
            }
            b'/' => {
                if bytes.get(j + 1) == Some(&b'>') {
                    self_closing = true;
                    terminated = true;
                    j += 2;
                    break;
                }
                j += 1;
                continue;
            }
            _ => {}
        }
        // 属性名
        let name_start = j;
        while j < bytes.len() && !is_html_space(bytes[j]) && !matches!(bytes[j], b'=' | b'/' | b'>') {
            j += 1;
        }
        let attr_name = html[name_start..j].to_ascii_lowercase();
        if attr_name.is_empty() {
            j += 1;
            continue;
        }
        // 等号
        while j < bytes.len() && is_html_space(bytes[j]) {
            j += 1;
        }
        let mut value = String::new();
        if j < bytes.len() && bytes[j] == b'=' {
            j += 1;
            while j < bytes.len() && is_html_space(bytes[j]) {
                j += 1;
            }
            if j < bytes.len() && (bytes[j] == b'"' || bytes[j] == b'\'') {
                let quote = bytes[j];
                j += 1;
                let value_start = j;
                while j < bytes.len() && bytes[j] != quote {
                    j += 1;
                }
                value = html[value_start..j].to_string();
                if j < bytes.len() {
                    j += 1;
                }
            } else {
                let value_start = j;
                while j < bytes.len() && !is_html_space(bytes[j]) && bytes[j] != b'>' {
                    j += 1;
                }
                value = html[value_start..j].to_string();
            }
        }
        let value = decode_entities(&normalize_newlines(&value), true);
        if let Some(index) = attr_index.as_mut() {
            if index.contains_key(&attr_name) {
                // 重复属性：**第一个**赢（后来的丢掉）
            } else {
                index.insert(attr_name.clone(), attrs.len());
                attrs.push((attr_name, value));
            }
        } else if attrs.iter().any(|(k, _)| *k == attr_name) {
            // 重复属性：第一个赢，值不覆盖
        } else {
            if attrs.len() >= 16 {
                let mut index = std::collections::HashMap::with_capacity(attrs.len() * 2);
                for (slot, (key, _)) in attrs.iter().enumerate() {
                    index.insert(key.clone(), slot);
                }
                index.insert(attr_name.clone(), attrs.len());
                attr_index = Some(index);
            }
            attrs.push((attr_name, value));
        }
    }
    (name, attrs, self_closing, terminated, j.min(bytes.len()))
}

fn is_html_space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r' | 0x0c)
}

/// 读 raw text / RCDATA 元素的内容，返回到它结束标签之后的字节位置。
///
/// 结束标签在**原串上大小写不敏感**地找：原来每遇到一个 `<script>` 就把整份 HTML `to_ascii_lowercase()`
/// 一遍，一整篇 2 MB、夹几百个脚本的输入会退化成 O(n²)。
fn read_raw_text(html: &str, from: usize, name: &str) -> (String, usize) {
    if name == "plaintext" {
        return (html[from..].to_string(), html.len());
    }
    let bytes = html.as_bytes();
    let name_bytes = name.as_bytes();
    let mut search = from;
    while let Some(rel) = html[search..].find('<') {
        let pos = search + rel;
        let after = pos + 2 + name_bytes.len();
        let matches = after <= bytes.len()
            && bytes[pos + 1] == b'/'
            && bytes[pos + 2..after].eq_ignore_ascii_case(name_bytes);
        if matches {
            let terminated = match html[after..].chars().next() {
                None => true,
                Some(c) => !c.is_ascii_alphanumeric() && c != '-' && c != '_' && c != ':' && c != '.',
            };
            if terminated {
                let end = match html[after..].find('>') {
                    Some(rel2) => after + rel2 + 1,
                    None => html.len(),
                };
                return (html[from..pos].to_string(), end);
            }
        }
        search = pos + 1;
    }
    (html[from..].to_string(), html.len())
}

/// HTML5 的输入预处理：`\r\n` / `\r` → `\n`。
pub fn normalize_newlines(s: &str) -> String {
    if !s.contains('\r') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\r' {
            if chars.peek() == Some(&'\n') {
                chars.next();
            }
            out.push('\n');
        } else {
            out.push(c);
        }
    }
    out
}

// ===========================================================================
// 实体
// ===========================================================================

/// 解**一遍**字符引用（HTML 分词器的口径，和 `bili::text::unescape_entities` 的"解到不变"不同）。
///
/// 三条口径：
/// - 文本里 `&ampfoo` → `&foo`、`&aacute` → `á`、`&notit;` → `¬it;`（命名实体按最长前缀，106 个 legacy 名可省分号）；
/// - **属性值里** legacy 名后面紧跟字母数字或 `=` 时**不解**（`&ampfoo` 原样、`&copy2` 原样）；
/// - 数字实体：`&#39;` / `&#x27;`，`0` 和代理区给 `?`，超出 U+10FFFF 给 U+FFFD，0x80..0x9F 走 Windows-1252。
pub fn decode_entities(s: &str, in_attribute: bool) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut i = 0usize;
    while i < s.len() {
        if s.as_bytes()[i] != b'&' {
            let c = s[i..].chars().next().expect("下标在边界内");
            out.push(c);
            i += c.len_utf8();
            continue;
        }
        if let Some((consumed, decoded)) = numeric_reference(&s[i + 1..]) {
            out.push_str(&decoded);
            i += 1 + consumed;
            continue;
        }
        if let Some((consumed, decoded, legacy)) = named_reference(&s[i + 1..]) {
            let allow = if in_attribute && legacy {
                // 属性值里：免分号的 legacy 名后面紧跟**名字字符**就不解。jsoup 1.18.3 实测的
                // 阻止集合正好是 `[-0-9=A-Z_a-z]`（`&copy-1` / `&copy_1` / `&copy1` / `&copy=1`
                // 都原样留着，`&copy)` / `&copy` / `&copy!` 都解掉）。
                let next = s[i + 1 + consumed..].chars().next();
                !matches!(next, Some(c) if c.is_ascii_alphanumeric() || matches!(c, '=' | '-' | '_'))
            } else {
                true
            };
            if allow {
                out.push_str(&decoded);
                i += 1 + consumed;
                continue;
            }
        }
        out.push('&');
        i += 1;
    }
    out
}

/// `&` 后面那截 → (吃掉的字节数, 解出来的文本)；不是数字实体就 `None`。
fn numeric_reference(rest: &str) -> Option<(usize, String)> {
    let after_hash = rest.strip_prefix('#')?;
    let (digits, radix, prefix_len) = match after_hash.strip_prefix(['x', 'X']) {
        Some(hex) => (hex, 16u32, 2usize),
        None => (after_hash, 10u32, 1usize),
    };
    let len = if radix == 16 {
        digits.bytes().take_while(u8::is_ascii_hexdigit).count()
    } else {
        digits.bytes().take_while(u8::is_ascii_digit).count()
    };
    if len == 0 {
        return None;
    }
    let mut value: u64 = 0;
    let mut overflow = false;
    for byte in digits.bytes().take(len) {
        let digit = char::from(byte).to_digit(radix).expect("已经按进制筛过") as u64;
        match value.checked_mul(u64::from(radix)).and_then(|v| v.checked_add(digit)) {
            Some(next) => value = next,
            None => {
                overflow = true;
                break;
            }
        }
    }
    let decoded = if overflow {
        "\u{FFFD}".to_string()
    } else {
        numeric_value_to_text(value)
    };
    // 结尾的 `;` 一并吃掉（`&#39;` 是 4 个字符，不是 3 个）
    let semicolon = usize::from(rest.as_bytes().get(prefix_len + len) == Some(&b';'));
    Some((prefix_len + len + semicolon, decoded))
}

fn numeric_value_to_text(value: u64) -> String {
    // Windows-1252 的 0x80..0x9F 修正表（和 jsoup / 浏览器一样）
    const CP1252: [u32; 32] = [
        0x20AC, 0x0081, 0x201A, 0x0192, 0x201E, 0x2026, 0x2020, 0x2021, 0x02C6, 0x2030, 0x0160, 0x2039, 0x0152,
        0x008D, 0x017D, 0x008F, 0x0090, 0x2018, 0x2019, 0x201C, 0x201D, 0x2022, 0x2013, 0x2014, 0x02DC, 0x2122,
        0x0161, 0x203A, 0x0153, 0x009D, 0x017E, 0x0178,
    ];
    let code = if (0x80..=0x9F).contains(&value) {
        CP1252[(value - 0x80) as usize]
    } else {
        value as u32
    };
    if code == 0 {
        // jsoup（和浏览器）**保留** NUL：`&#0;` 解出来就是 U+0000，不是替换符也不是 `?`。
        //
        // 这一条原来是 `"?"`，实测和 jsoup 不一致：`<p>a&#0;b</p>` 在 jsoup 里是 "a\0b"。
        // 而且 `?` 会让 `<p>&#0;</p>` **多出一个段落**（`?` 不是空白，段落没被丢掉），
        // 这个差异在差分测试里占了一大类。
        return "\u{0}".to_string();
    }
    if (0xD800..=0xDFFF).contains(&code) {
        // 孤立代理点：Rust 的 `String` 里装不下（JS 字符串可以），只能替换。
        // 这是**跨语言的表示限制**，不是解析逻辑：差分测试把它们当已知差异。
        return "\u{FFFD}".to_string();
    }
    match char::from_u32(code) {
        Some(c) => c.to_string(),
        None => "\u{FFFD}".to_string(),
    }
}

/// `&` 后面那截 → (吃掉的字节数, 解码文本, 是不是不带分号的 legacy 名)。不是命名实体就 `None`。
fn named_reference(rest: &str) -> Option<(usize, String, bool)> {
    // 名字只能由字母数字和结尾的 `;` 组成；最长前缀匹配，所以按最长可能长度往回缩着试
    let bytes = rest.as_bytes();
    let mut max = 0usize;
    while max < bytes.len() && max < entities::MAX_ENTITY_NAME_LEN + 1 {
        let b = bytes[max];
        if b.is_ascii_alphanumeric() {
            max += 1;
        } else {
            break;
        }
    }
    if max == 0 {
        return None;
    }
    // 先试"带分号"的形态（长度 max+1），再一路缩到 1
    let mut end = if bytes.get(max) == Some(&b';') { max + 1 } else { max };
    while end > 0 {
        let name = &rest[..end];
        if let Some(value) = entities::lookup(name) {
            let legacy = !name.ends_with(';');
            return Some((end, value.to_string(), legacy));
        }
        end -= 1;
    }
    None
}

// ===========================================================================
// 入口
// ===========================================================================

/// `Jsoup.parseBodyFragment(html, …)` 的替身：返回一个 `#root` 元素，孩子就是片段的顶层节点。
pub fn parse_body_fragment(html: &str) -> Element {
    let mut builder = TreeBuilder::new();
    builder.run(html);
    builder.finish()
}

