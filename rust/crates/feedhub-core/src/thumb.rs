//! 卡片 / 表情 / 评论图的**缩略图地址改写**（`app/.../feed/CardThumbnails.kt` 的 Rust 版）。
//!
//! 那个理由自然没了。
//!
//! 逐条列在下面这张表里
//! （和 Kotlin 原版 KDoc 同一份表，改口径两边一起改）：
//!
//! | 平台 | 写法 |
//! |---|---|
//! | B站 `hdslb.com` | `原图@{宽}w.webp` |
//! | 知乎 `zhimg.com` | 文件名后缀换成 `_720w` / `_1440w` |
//! | 小黑盒 `max-c.com` | `?imageMogr2/thumbnail/{宽}x>`（只缩不放），**只认 `imgheybox*` 那台** |
//! | 酷安 `image.coolapk.com` | `原图.s.jpg`（固定 720 宽；卡片比 720 宽就用原图） |
//! | X `pbs.twimg.com` | `?format=…&name=medium`（卡片更宽用 `large`） |
//!
//! 不改、照旧用平台给的地址：贴吧（`bigPic` 本来就是 `w=720;q=60`）、抖音 / TikTok（地址带签名）、
//! Reddit（`preview.redd.it` 带 `s=`）、YouTube（已经是 `hq720`）、小红书（`!样式` 后缀）。
//!
//! ## 和 Kotlin 原版的口径（为什么这些地方是手写而不是正则）
//!
//! Kotlin 那版用 7 条 `Regex`（Java 的 `\w` / `\d` 默认就是 ASCII，`IGNORE_CASE` 只对 ASCII 字母有意义）。
//! 这里手写，**不引 regex crate**（`.so` 里多一个正则引擎不值，而且 Java 的 `\w` 不是 Unicode 口径，
//! 用 regex crate 还得逐条改）。唯一没复刻的是 Java `$` 对"结尾换行"的那条怪癖：URL 里不会有换行，
//! 真出现了这边当"不匹配"（Kotlin 那边可能匹配掉那个换行）——对拍夹具里没有这种输入，

use std::fmt::Write as _;

/// 酷安"卡片比这宽就用原图"的那条线（`.s.jpg` 固定 720 宽）。
const COOLAPK_SMALL: i32 = 720;

// ===========================================================================
// 卡片宽度档
// ===========================================================================

/// 卡片宽度向上取到 160 的倍数（窗口拉一点点不换地址、缓存还能用），夹在 320～1920。
///
/// Kotlin 原版是 `(ceil(w / 160.0).toInt() * 160).coerceIn(320, 1920)`：`toInt()` 先截断、
/// 再按 **Int 溢出回绕**乘 160（`w` 极大时回绕成负数，最后夹成 320）。这里照着用 `i32` 包装乘法，
/// 免得在离谱的输入上两边给出不同答案。
pub fn bucket(card_width_px: i32) -> i32 {
    let steps = (card_width_px.max(1) as f64 / 160.0).ceil() as i32;
    steps.wrapping_mul(160).clamp(320, 1920)
}

// ===========================================================================
// 地址改写
// ===========================================================================

/// 卡片该用的地址：认得的 CDN 换成不比 `width_px` 窄的缩略图，其余原样。
pub fn of(url: &str, width_px: i32) -> String {
    let path = before(url, '?');
    if ends_with_ignore_case(path, ".gif") {
        return url.to_string();
    }
    // B站：`@` 后面就是 CDN 的处理参数，已经带了的不动
    if url.contains("hdslb.com/") {
        return if after_last(path, '/').contains('@') {
            url.to_string()
        } else {
            format!("{url}@{width_px}w.webp")
        };
    }
    // 知乎：`v2-<哈希>_<尺寸>.<扩展名>[?参数]`，只换尺寸那一段
    if url.contains("zhimg.com/") {
        return match zhimg_match(url) {
            Some((prefix, ext, query)) => {
                let size = if width_px <= COOLAPK_SMALL { "720w" } else { "1440w" };
                format!("{prefix}_{size}.{ext}{query}")
            }
            // 不认识的形状：原样（Kotlin 那边 `Regex.replace` 没命中就不会变）
            None => url.to_string(),
        };
    }
    // 酷安：`.s.jpg` 是缩过的；卡片比 720 宽 / 已经是小图 / 不是静态图就不动
    if url.contains("image.coolapk.com/") {
        let already = path.ends_with(".s.jpg") || path.ends_with(".m.jpg") || path.ends_with(".xs.jpg");
        return if width_px > COOLAPK_SMALL || already || !static_ext_match(path) {
            url.to_string()
        } else {
            // 注意：查询串在这里被**丢掉**（和 Kotlin 原版一致：用的是 `path`）
            format!("{path}.s.jpg")
        };
    }
    // 小黑盒：只认 `imgheybox*` 那台图片机；带过参数的不动
    if url.contains(".max-c.com/") && heybox_img_host(url) {
        return if url.contains('?') {
            url.to_string()
        } else {
            format!("{url}?imageMogr2/thumbnail/{width_px}x%3E")
        };
    }
    // X：`media/<id>[.ext][?…]` → `?format=…&name=medium|large`
    if url.contains("pbs.twimg.com/media/") {
        if let Some((id, ext)) = twimg_match(url) {
            let format = if !ext.is_empty() {
                ext
            } else {
                format_param(url).unwrap_or("jpg")
            };
            let name = if width_px <= 900 { "medium" } else { "large" };
            return format!("https://pbs.twimg.com/media/{id}?format={format}&name={name}");
        }
    }
    url.to_string()
}

/// 这张图**能不能**拿缩略图预取（九宫格第 2～9 张）：拿不准是不是原图的一律 `None`。
///
/// 和 [`of`] 分开是因为卡片上"必须有一张图"（拿不到缩略图就用原图），而**预取**宁可不取也不能把原图拉下来。
pub fn thumbnail_for(url: &str, width_px: i32) -> Option<String> {
    if url.is_empty() {
        return None;
    }
    // 动图：缩略图写法会把它变成静态的（[`of`] 也是原样返回），预取它没意义
    if ends_with_ignore_case(before(url, '?'), ".gif") {
        return None;
    }
    let rewritten = of(url, width_px);
    if rewritten != url {
        return Some(rewritten);
    }
    if is_server_thumbnail(url) {
        Some(url.to_string())
    } else {
        None
    }
}

/// 一批（**一屏卡片只过一次 FFI**）：和逐条调 [`of`] 结果完全一样，去重 / 保序留给调用方。
pub fn of_many(urls: &[String], width_px: i32) -> Vec<String> {
    urls.iter().map(|u| of(u, width_px)).collect()
}

/// 一批 [`thumbnail_for`]（`None` = 这张不预取）。
pub fn thumbnail_for_many(urls: &[String], width_px: i32) -> Vec<Option<String>> {
    urls.iter().map(|u| thumbnail_for(u, width_px)).collect()
}

/// **同一个地址、好几个宽度档**：每一档改写一次、去重、保序（封面向量试缓存用，先 640 再几档退路）。
///
/// 和 [`of`] 逐档调一样的结果，只是去重也在这里做（Kotlin 那边少一次 `distinct()`）。
pub fn of_widths(url: &str, widths: &[i32]) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(widths.len());
    for w in widths {
        let candidate = of(url, *w);
        if !out.contains(&candidate) {
            out.push(candidate);
        }
    }
    out
}

/// 一批 [`identity`]（详情页拿卡片缩略图当占位时，一次比一整屏）。
pub fn identity_many(urls: &[String]) -> Vec<String> {
    urls.iter().map(|u| identity(u)).collect()
}

/// 地址看起来**已经是**服务端缩好的那一档（不是原图）。只认在线核实过的那几种写法，拿不准就不算。
pub fn is_server_thumbnail(url: &str) -> bool {
    let path = before(before(url, '?'), '#');
    let after_scheme = after(url, "://");
    let host = before(after_scheme, '/').to_lowercase();
    if host.contains("hdslb.com") {
        // B站：`a.jpg@640w.webp`
        return after_last(path, '/').contains('@');
    }
    if host.contains("zhimg.com") {
        // 知乎：`v2-<哈希>_720w.jpg`（没带尺寸那一段的是原图）
        return after_last(path, '/').contains('_');
    }
    if host.contains("image.coolapk.com") {
        return coolapk_small_ext_match(path);
    }
    if host.contains(".max-c.com") {
        return url.contains('?');
    }
    if host.contains("pbs.twimg.com") {
        return url.contains("name=");
    }
    if host.contains("tiebapic.baidu.com") || host.contains("hiphotos.baidu.com") {
        return tieba_size_match(url) || after_char(url, '?').contains("w=");
    }
    if host.contains("xhscdn.com") || host.contains("xiaohongshu.com") {
        return path.contains('!');
    }
    if host.contains("douyinpic.com")
        || host.contains("douyin.com")
        || host.contains("byteimg.com")
        || host.contains("tiktokcdn")
    {
        return path.contains("~tplv-");
    }
    if host.contains("redd.it") {
        return host.starts_with("preview.") || host.starts_with("external-preview.");
    }
    if host.contains("ytimg.com") {
        return yt_video_match(path).is_some();
    }
    false
}

/// 同一张图的两种写法 → 同一个"身份"（详情页拿卡片上的缩略图当占位时用）。
///
/// 认得的 CDN 取文件名 / 哈希，认不得的退回"主机 + 路径、去掉查询串"。比不上的最多是不用占位。
pub fn identity(url: &str) -> String {
    let no_query = before(before(url, '?'), '#');
    let after_scheme = after(no_query, "://");
    let host = before(after_scheme, '/').to_lowercase();
    // `substringAfter('/', "")`：**没有**就算空串（认不得的主机路径为空，身份就只剩主机）
    let path = after_or_empty(after_scheme, "/");
    if host.contains("hdslb.com") || host.contains("hdslb.cn") {
        return format!("hdslb:{}", before(path, '@'));
    }
    if host.contains("zhimg.com") {
        return match zhimg_id_match(path) {
            Some(id) => format!("zhimg:{id}"),
            None => format!("zhimg:{host}/{path}"),
        };
    }
    if host.contains(".max-c.com") {
        return format!("heybox:{path}");
    }
    if host.contains("image.coolapk.com") {
        return format!("coolapk:{}", coolapk_small_ext_strip(path));
    }
    if host.contains("pbs.twimg.com") {
        return match twimg_id_match(path) {
            Some(id) => format!("twimg:{id}"),
            None => format!("twimg:{path}"),
        };
    }
    if host.contains("tiebapic.baidu.com") || host.contains("hiphotos.baidu.com") {
        // 尺寸段和 sign 段每次都可能不一样
        let kept: Vec<&str> = path
            .split('/')
            .filter(|seg| {
                !(seg.starts_with("sign=")
                    || starts_with_ignore_case(seg, "w%3D")
                    || starts_with_ignore_case(seg, "w="))
            })
            .collect();
        return format!("tieba:{}", kept.join("/"));
    }
    if host.contains("douyinpic.com")
        || host.contains("douyin.com")
        || host.contains("byteimg.com")
        || host.contains("tiktokcdn")
    {
        return format!("byte:{}", before(path, '~'));
    }
    if host.contains("xhscdn.com") || host.contains("xiaohongshu.com") {
        return format!("xhs:{}", before(path, '!'));
    }
    if host.contains("ytimg.com") {
        return match yt_video_match(path) {
            Some(id) => format!("yt:{id}"),
            None => format!("yt:{path}"),
        };
    }
    if host.contains("redd.it") {
        return format!("reddit:{host}/{path}");
    }
    format!("url:{host}/{path}")
}

// ===========================================================================
// Kotlin 字符串口径（和 `CardThumbnails.kt` 里的 `substringBefore` / `substringAfter` 一一对应）
// ===========================================================================

/// `s.substringBefore(c)`。
fn before(s: &str, c: char) -> &str {
    match s.find(c) {
        Some(i) => &s[..i],
        None => s,
    }
}

/// `s.substringBefore(pat)`（`pat` 是字符串时的那一版，例如 `substringBefore(".max-c.com/")`）。
fn before_str<'a>(s: &'a str, pat: &str) -> &'a str {
    match s.find(pat) {
        Some(i) => &s[..i],
        None => s,
    }
}

/// `s.substringAfter(pat)`（没有 `pat` 时返回整串，和 Kotlin 的两参默认值 `""` **不一样**：见 [`after_or_empty`]）。
fn after<'a>(s: &'a str, pat: &str) -> &'a str {
    match s.find(pat) {
        Some(i) => &s[i + pat.len()..],
        None => s,
    }
}

/// `s.substringAfter(c)`（字符那一版）。
fn after_char(s: &str, c: char) -> &str {
    match s.find(c) {
        Some(i) => &s[i + c.len_utf8()..],
        None => s,
    }
}

/// `s.substringAfter(pat, "")`：没有 `pat` 时返回**空串**（Kotlin `identity` 里取路径用的就是这个）。
fn after_or_empty<'a>(s: &'a str, pat: &str) -> &'a str {
    match s.find(pat) {
        Some(i) => &s[i + pat.len()..],
        None => "",
    }
}

/// `s.substringAfterLast(c)`。
fn after_last(s: &str, c: char) -> &str {
    match s.rfind(c) {
        Some(i) => &s[i + c.len_utf8()..],
        None => s,
    }
}

/// Java 的 `\w`（**ASCII**：`[a-zA-Z_0-9]`，不是 Unicode 那一套）。
fn is_java_word(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

fn starts_with_ignore_case(s: &str, prefix: &str) -> bool {
    let (sb, pb) = (s.as_bytes(), prefix.as_bytes());
    pb.len() <= sb.len()
        && sb[..pb.len()]
            .iter()
            .zip(pb)
            .all(|(a, b)| a.to_ascii_lowercase() == b.to_ascii_lowercase())
}

fn ends_with_ignore_case(s: &str, suffix: &str) -> bool {
    let (sb, fb) = (s.as_bytes(), suffix.as_bytes());
    fb.len() <= sb.len()
        && sb[sb.len() - fb.len()..]
            .iter()
            .zip(fb)
            .all(|(a, b)| a.to_ascii_lowercase() == b.to_ascii_lowercase())
}

// ===========================================================================
// 原来那 7 条正则
// ===========================================================================

/// `\.(jpe?g|png|webp)$`（IGNORE_CASE）。
fn static_ext_match(path: &str) -> bool {
    ["jpeg", "jpg", "png", "webp"]
        .iter()
        .any(|ext| ends_with_ignore_case(path, &format!(".{ext}")))
}

/// `\.(?:x?s|m)\.jpg$`（IGNORE_CASE）：酷安的小图后缀。
fn coolapk_small_ext_match(path: &str) -> bool {
    [".s.jpg", ".xs.jpg", ".m.jpg"]
        .iter()
        .any(|ext| ends_with_ignore_case(path, ext))
}

/// `COOLAPK_SMALL_EXT.replace(path, "")`：把结尾那个小图后缀去掉（没有就原样）。
fn coolapk_small_ext_strip(path: &str) -> &str {
    for ext in [".xs.jpg", ".s.jpg", ".m.jpg"] {
        if ends_with_ignore_case(path, ext) {
            return &path[..path.len() - ext.len()];
        }
    }
    path
}

/// `^(.*/v2-[0-9a-f]+)_(\w+)\.(jpe?g|png|webp)(\?.*)?$`（IGNORE_CASE，贪婪 `.*`）。
///
/// 返回 `(前缀含 v2-<hex>, 扩展名原文, 查询串含 ? 或空)`；不匹配返回 `None`。
fn zhimg_match(url: &str) -> Option<(&str, &str, &str)> {
    // 贪婪 `.*` 等价于"从最靠右的那个 `/v2-` 开始试，失败了往左退"。
    let mut limit = url.len();
    loop {
        let pos = url[..limit].rfind("/v2-")?;
        let mut i = pos + 4;
        let bytes = url.as_bytes();
        while i < bytes.len() && bytes[i].is_ascii_hexdigit() {
            i += 1;
        }
        if i > pos + 4 {
            if let Some((ext, query)) = zhimg_tail(&url[i..]) {
                return Some((&url[..i], ext, query));
            }
        }
        if pos == 0 {
            return None;
        }
        limit = pos;
    }
}

/// `_(\w+)\.(jpe?g|png|webp)(\?.*)?$` 那一段。
fn zhimg_tail(s: &str) -> Option<(&str, &str)> {
    let b = s.as_bytes();
    if b.first() != Some(&b'_') {
        return None;
    }
    let mut i = 1;
    let word_start = i;
    while i < b.len() && is_java_word(b[i]) {
        i += 1;
    }
    if i == word_start || b.get(i) != Some(&b'.') {
        return None;
    }
    i += 1;
    for cand in ["jpeg", "jpg", "png", "webp"] {
        if starts_with_ignore_case(&s[i..], cand) {
            let end = i + cand.len();
            let tail = &s[end..];
            if tail.is_empty() {
                return Some((&s[i..end], ""));
            }
            if tail.starts_with('?') {
                return Some((&s[i..end], tail));
            }
        }
    }
    None
}

/// `v2-[0-9a-f]+`（IGNORE_CASE）的第一次出现。
fn zhimg_id_match(path: &str) -> Option<&str> {
    let bytes = path.as_bytes();
    let mut from = 0;
    while let Some(rel) = path[from..].find("v2-") {
        let start = from + rel;
        let mut i = start + 3;
        while i < bytes.len() && bytes[i].is_ascii_hexdigit() {
            i += 1;
        }
        if i > start + 3 {
            return Some(&path[start..i]);
        }
        from = start + 3;
        if from >= path.len() {
            return None;
        }
    }
    None
}

/// `^https?://pbs\.twimg\.com/media/([A-Za-z0-9_-]+)(?:\.(\w+))?(?:\?.*)?$`。
fn twimg_match(url: &str) -> Option<(&str, &str)> {
    let rest = url
        .strip_prefix("https://pbs.twimg.com/media/")
        .or_else(|| url.strip_prefix("http://pbs.twimg.com/media/"))?;
    let bytes = rest.as_bytes();
    let mut i = 0;
    while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_' || bytes[i] == b'-') {
        i += 1;
    }
    if i == 0 {
        return None;
    }
    let id = &rest[..i];
    let tail = &rest[i..];
    if tail.is_empty() {
        return Some((id, ""));
    }
    if tail.starts_with('?') {
        return Some((id, ""));
    }
    // `.(\w+)` 之后只剩"结尾"或"查询串"
    let after_dot = tail.strip_prefix('.')?;
    let tb = after_dot.as_bytes();
    let mut j = 0;
    while j < tb.len() && is_java_word(tb[j]) {
        j += 1;
    }
    if j == 0 {
        return None;
    }
    let ext = &after_dot[..j];
    let rest2 = &after_dot[j..];
    if rest2.is_empty() || rest2.starts_with('?') {
        Some((id, ext))
    } else {
        None
    }
}

/// `Regex("format=(\\w+)")` 的第一次出现（`of` 里给没有扩展名的 `media/<id>` 兜底用）。
fn format_param(url: &str) -> Option<&str> {
    let bytes = url.as_bytes();
    let mut from = 0;
    while let Some(rel) = url[from..].find("format=") {
        let start = from + rel + "format=".len();
        let mut i = start;
        while i < bytes.len() && is_java_word(bytes[i]) {
            i += 1;
        }
        if i > start {
            return Some(&url[start..i]);
        }
        from = start;
        if from >= url.len() {
            return None;
        }
    }
    None
}

/// `(?:^|/)media/([A-Za-z0-9_-]+)` 的第一次出现。
fn twimg_id_match(path: &str) -> Option<&str> {
    find_path_id(path, "media/")
}

/// `(?:^|/)vi/([A-Za-z0-9_-]+)` 的第一次出现。
fn yt_video_match(path: &str) -> Option<&str> {
    find_path_id(path, "vi/")
}

/// `(?:^|/)<marker>([A-Za-z0-9_-]+)`：从最靠左的、前面是行首或 `/` 的那一处取一段 id。
fn find_path_id<'a>(path: &'a str, marker: &str) -> Option<&'a str> {
    let bytes = path.as_bytes();
    let mut from = 0;
    while let Some(rel) = path[from..].find(marker) {
        let pos = from + rel;
        let at_boundary = pos == 0 || bytes[pos - 1] == b'/';
        if at_boundary {
            let start = pos + marker.len();
            let mut i = start;
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_' || bytes[i] == b'-') {
                i += 1;
            }
            if i > start {
                return Some(&path[start..i]);
            }
        }
        from = pos + marker.len();
        if from >= path.len() {
            return None;
        }
    }
    None
}

/// `w%3D\d+%3Bq%3D|w=\d+;q=`（IGNORE_CASE）：贴吧地址里的尺寸段。
///
/// 大小写不敏感在**只把 ASCII 字母小写化的副本**上做（`make_ascii_lowercase` 逐字节、不改长度和边界）：
/// `str::to_lowercase` 在个别 Unicode 字符上会变长，那样下标就和原串对不上了。
fn tieba_size_match(url: &str) -> bool {
    let mut lowered = url.as_bytes().to_vec();
    lowered.make_ascii_lowercase();
    let hay = std::str::from_utf8(&lowered).unwrap_or(url);
    for (prefix, tail) in [("w%3d", "%3bq%3d"), ("w=", ";q=")] {
        let bytes = hay.as_bytes();
        let mut from = 0;
        while let Some(rel) = hay[from..].find(prefix) {
            let start = from + rel + prefix.len();
            let mut i = start;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
            if i > start && hay[i..].starts_with(tail) {
                return true;
            }
            from = start;
            if from >= hay.len() {
                break;
            }
        }
    }
    false
}

/// 小黑盒那台图片机：`imgheybox…` 开头（`url.substringBefore(".max-c.com/").substringAfter("//")`）。
fn heybox_img_host(url: &str) -> bool {
    after(before_str(url, ".max-c.com/"), "//").starts_with("imgheybox")
}

/// 调试 / 日志用：把一批地址改写的结果拼成一行（**不含**任何用户内容，只有地址）。
pub fn summarize(urls: &[String]) -> String {
    let mut out = String::new();
    let mut changed = 0usize;
    for u in urls {
        if of(u, 640) != *u {
            changed += 1;
        }
    }
    let _ = write!(out, "{changed}/{}", urls.len());
    out
}

