//! `x-client-transaction-id` 的**算法**（移植自 QuaX 的 Dart 实现，MIT；算法来源
//! iSarabjitDhiman/XClientTransaction，MIT。版权声明全文见 `source-x/.../XClientTransaction.kt`）。
//!
//! 这一块**只做纯计算**：给定页面 HTML + sign 模块正文（外加随机关键字 / 常量），算出三样输入
//! （48 字节 key、动画 key、随机数），再按"方法 + path + 秒 + 随机字节"生成一次请求用的 id。
//!
//! **调用时机不在这里**：`x-client-transaction-id` 必须在**限速之后、真要发出去那一刻**现算
//! ，所以宿主的 `TxidInterceptor` 在 OkHttp 的拦截器链里调
//! `generate`；平台对象（[`super::source`]）拼请求时**不带**这个头。
//!
//! 和 Kotlin 版的分工：
//! - 搬过来的：`XClientTransaction.fromSources`（HTML / sign 模块里抠输入）、`generateTransactionId`、
//!   `XtxMath`（jsRound / roundTo2 / isOdd / floatToHex / interpolate / Cubic / 旋转矩阵）；
//! - **没搬**：`XSignChain`（"去哪个脚本里找输入"那三跳，含取页面 / 脚本的请求）。理由是它本身就是
//!   **另一套宿主请求**（`pageClient`：页面头 + 1 秒那档限速 + 渠道规则可被 `ctx.store` 覆盖），
//!   而它的产物就是这里要的两份文本 —— 所以边界切在"两份文本进来"。

/// X 的时间纪元（秒），
pub const EPOCH_SECONDS: i64 = 1_682_924_400;

/// sign 模块里的固定关键字（`obfiowerehiring`）。
pub const DEFAULT_KEYWORD: &str = "obfiowerehiring";

/// 追加的随机数（QuaX 的 `ADDITIONAL_RANDOM_NUMBER`）。
pub const ADDITIONAL_RANDOM_NUMBER: u8 = 3;

/// 已经抠好输入的生成器（`keyBytes` / `animationKey` / 关键字 / 随机数）。
#[derive(Debug, Clone)]
pub struct XTransaction {
    key_bytes: Vec<u8>,
    animation_key: String,
    random_keyword: String,
    random_number: u8,
}

impl XTransaction {
    /// 从"页面 HTML + sign 模块正文"构造（离线测试走这条路；生产里由宿主取回来后调它）。
    pub fn from_sources(
        page_html: &str,
        sign_file_text: &str,
        random_keyword: &str,
        random_number: u8,
    ) -> Result<Self, String> {
        let indices = get_indices(sign_file_text)?;
        let row_index = indices[0];
        let key = get_key(page_html)?;
        let key_bytes = base64_decode(&key)
            .ok_or_else(|| "[twitter-site-verification] 的 content 不是合法 base64".to_string())?;
        let animation_key =
            compute_animation_key(&key_bytes, row_index, &indices[1..], page_html)?;
        Ok(Self {
            key_bytes,
            animation_key,
            random_keyword: random_keyword.to_string(),
            random_number,
        })
    }

    /// 生成一次请求用的 id。[`path`] 是 URL 的 path（**不含 query**）；[`method`] 用大写。
    /// [`random_byte`] 真实调用传随机值，测试传固定值。
    pub fn generate(&self, method: &str, path: &str, now_ms: i64, random_byte: u8) -> String {
        let time_now = (now_ms - EPOCH_SECONDS * 1000) / 1000;
        let time_now_bytes = [
            (time_now & 0xFF) as u8,
            ((time_now >> 8) & 0xFF) as u8,
            ((time_now >> 16) & 0xFF) as u8,
            ((time_now >> 24) & 0xFF) as u8,
        ];

        let hash_input = format!(
            "{method}!{path}!{time_now}{}{}",
            self.random_keyword, self.animation_key
        );
        let hash = sha256(hash_input.as_bytes());

        let mut body: Vec<u8> = Vec::with_capacity(self.key_bytes.len() + 4 + 16 + 1);
        body.extend_from_slice(&self.key_bytes);
        body.extend_from_slice(&time_now_bytes);
        body.extend_from_slice(&hash[..16]);
        body.push(self.random_number);

        let mut out = Vec::with_capacity(body.len() + 1);
        out.push(random_byte);
        for b in body {
            out.push(b ^ random_byte);
        }
        base64_encode(&out)
    }

    /// 抠出来的 48 字节 key（诊断 / 测试用：**不要打印**整份页面）。
    pub fn key_bytes(&self) -> &[u8] {
        &self.key_bytes
    }

    /// 算出来的动画 key（诊断 / 测试用）。
    pub fn animation_key(&self) -> &str {
        &self.animation_key
    }
}

// ===========================================================================
// 从 HTML / JS 里抠输入
// ===========================================================================

/// sign 模块里的下标正则（对齐 Kotlin `XClientTransaction.INDICES_REGEX`：
/// `(\(\w\[(\d{1,2})\],\s*16\))+`）。返回每个匹配里**最后一个**下标。
///
/// 注意：Kotlin 的正则带 `+`，连续的几次重复会被当成**同一个匹配**、只留下最后一次的下标。
/// 这里逐个匹配（夹具里 4 个下标各是一个匹配，结果一样）；差别只在"两次重复紧挨着"的形态上，
fn get_indices(text: &str) -> Result<Vec<usize>, String> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'(' {
            i += 1;
            continue;
        }
        let mut j = i + 1;
        if j >= bytes.len() || !(bytes[j].is_ascii_alphanumeric() || bytes[j] == b'_') {
            i += 1;
            continue;
        }
        j += 1;
        if j >= bytes.len() || bytes[j] != b'[' {
            i += 1;
            continue;
        }
        j += 1;
        let digits_start = j;
        while j < bytes.len() && bytes[j].is_ascii_digit() && j - digits_start < 2 {
            j += 1;
        }
        if j == digits_start || (j < bytes.len() && bytes[j].is_ascii_digit()) {
            i += 1;
            continue;
        }
        let digits = &text[digits_start..j];
        if j >= bytes.len() || bytes[j] != b']' {
            i += 1;
            continue;
        }
        j += 1;
        if j >= bytes.len() || bytes[j] != b',' {
            i += 1;
            continue;
        }
        j += 1;
        while j < bytes.len() && matches!(bytes[j], b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c) {
            j += 1;
        }
        if !text[j..].starts_with("16") {
            i += 1;
            continue;
        }
        j += 2;
        if j >= bytes.len() || bytes[j] != b')' {
            i += 1;
            continue;
        }
        let index: usize = digits
            .parse()
            .map_err(|_| format!("sign 模块里的下标不是数字：{digits}"))?;
        out.push(index);
        i = j + 1;
    }
    if out.is_empty() {
        return Err(
            "sign 模块里没找到 KEY_BYTE 下标（正则 (\\w[N], 16) 一条都没命中）".to_string(),
        );
    }
    Ok(out)
}

/// 页面里 `<meta name="twitter-site-verification" content="…">` 的 content。
fn get_key(html: &str) -> Result<String, String> {
    for tag in tags(html) {
        if !tag.name.eq_ignore_ascii_case("meta") {
            continue;
        }
        if tag.attr("name").as_deref() != Some("twitter-site-verification") {
            continue;
        }
        let content = tag.attr("content").unwrap_or_default();
        if content.trim().is_empty() {
            return Err("[twitter-site-verification] 的 content 是空的".to_string());
        }
        return Ok(content);
    }
    Err("页面里没有 [twitter-site-verification] 这个 meta".to_string())
}

/// 一帧 SVG 的动画数据（`d` 属性 → 二维整数数组）。
fn get_2d_array(key_bytes: &[u8], html: &str) -> Result<Vec<Vec<f64>>, String> {
    let frames = frame_elements(html);
    if frames.is_empty() {
        return Err("页面里没有 loading-x-anim 动画帧".to_string());
    }
    let index = key_bytes.get(5).copied().unwrap_or(0) as usize % 4;
    let frame = frames
        .get(index)
        .ok_or_else(|| format!("loading-x-anim 帧不够 4 个（只有 {} 个）", frames.len()))?;
    let d = frame
        .paths
        .get(1)
        .ok_or_else(|| "loading-x-anim 帧里没有第二条 path".to_string())?;
    if d.chars().count() <= 9 {
        return Err("动画帧的 path 太短，读不出坐标".to_string());
    }
    // Kotlin `d.substring(9)`：跳过前的 "M x,y C" 前缀（按 UTF-16 下标，这里全是 ASCII）
    let skip: usize = d.char_indices().nth(9).map(|(i, _)| i).unwrap_or(d.len());
    let mut out = Vec::new();
    for segment in d[skip..].split('C') {
        let numbers = canonicalize_ints(segment)?;
        if !numbers.is_empty() {
            out.push(numbers);
        }
    }
    Ok(out)
}

/// `canonicalize(segment).split(' ').filter { it.isNotEmpty() }.map { it.toInt() }`。
fn canonicalize_ints(segment: &str) -> Result<Vec<f64>, String> {
    let mut out = Vec::new();
    let mut current = String::new();
    for c in segment.chars() {
        if c.is_ascii_digit() {
            current.push(c);
        } else if !current.is_empty() {
            out.push(parse_number(&current)?);
            current.clear();
        }
    }
    if !current.is_empty() {
        out.push(parse_number(&current)?);
    }
    Ok(out)
}

fn parse_number(text: &str) -> Result<f64, String> {
    text.parse::<f64>()
        .map_err(|_| format!("动画帧里的坐标不是数字：{text}"))
}

// ===========================================================================
// 动画 key
// ===========================================================================

fn compute_animation_key(
    key_bytes: &[u8],
    row_index: usize,
    key_bytes_indices: &[usize],
    html: &str,
) -> Result<String, String> {
    let total_time = 4096.0;
    let row_byte = *key_bytes
        .get(row_index)
        .ok_or_else(|| format!("keyBytes 里没有第 {row_index} 个字节"))?;
    let frame_row_index = (row_byte % 16) as usize;
    let mut product: i64 = 1;
    for idx in key_bytes_indices {
        let b = *key_bytes
            .get(*idx)
            .ok_or_else(|| format!("keyBytes 里没有第 {idx} 个字节"))?;
        product *= (b % 16) as i64;
    }
    let frame_time = js_round(product as f64 / 10.0) * 10.0;

    let arr = get_2d_array(key_bytes, html)?;
    let frame_row = arr
        .get(frame_row_index)
        .ok_or_else(|| format!("动画帧里没有第 {frame_row_index} 行"))?;
    animate(frame_row, frame_time / total_time)
}

/// 一帧 → 动画 key（十六进制串）。逐行照 Kotlin `animate`。
fn animate(frames: &[f64], target_time: f64) -> Result<String, String> {
    if frames.len() < 7 {
        return Err(format!("动画帧的列数不够（只有 {} 个）", frames.len()));
    }
    let from_color = vec![frames[0], frames[1], frames[2], 1.0];
    let to_color = vec![frames[3], frames[4], frames[5], 1.0];
    let from_rotation = vec![0.0];
    let to_rotation = vec![solve(frames[6], 60.0, 360.0, true)];

    let mut curves = Vec::with_capacity(frames.len() - 7);
    for (i, v) in frames[7..].iter().enumerate() {
        curves.push(solve(*v, is_odd(i), 1.0, false));
    }
    let cubic = Cubic::new(curves);
    let value = cubic.value(target_time)?;

    let color: Vec<f64> = interpolate(&from_color, &to_color, value)?
        .into_iter()
        .map(|c| c.clamp(0.0, 255.0))
        .collect();
    let rotation = interpolate(&from_rotation, &to_rotation, value)?;
    let matrix = convert_rotation_to_matrix(rotation[0]);

    let mut parts: Vec<String> = Vec::with_capacity(color.len() + matrix.len());
    for c in color.iter().take(color.len() - 1) {
        parts.push(format!("{:x}", dart_round(*c)));
    }
    for v in matrix {
        let mut rounded = round_to_2(v);
        if rounded < 0.0 {
            rounded = -rounded;
        }
        let hex = float_to_hex(rounded);
        if let Some(rest) = hex.strip_prefix('.') {
            // Kotlin 是 `"0$hex".lowercase()`；这里的 hex 只含 0-9 / A-F / '.'，ASCII 小写就够，
            // 别用 `to_lowercase()`（会把整张 Unicode 大小写表拉进 .so）
            parts.push(format!("0.{rest}").to_ascii_lowercase());
        } else if !hex.is_empty() {
            parts.push(hex);
        } else {
            parts.push("0".to_string());
        }
    }
    parts.push("0".to_string());
    parts.push("0".to_string());
    Ok(parts.concat().replace(['.', '-'], ""))
}

/// 一帧里的一个值 → 目标区间的实数（对齐 QuaX `solve`）。
fn solve(value: f64, min_val: f64, max_val: f64, rounding: bool) -> f64 {
    let result = value * (max_val - min_val) / 255.0 + min_val;
    if rounding {
        result.floor()
    } else {
        round_to_2(result)
    }
}

/// Dart 的 `double.round()`：半数远离零。颜色分量非负，实际就是 floor(x+0.5)。
fn dart_round(v: f64) -> i64 {
    (v + 0.5).floor() as i64
}

/// JS 的 `Math.round`（.5 向正无穷，-2.5 → -2）。**别换成 Rust 的 `round()`**（那是半数远离零）。
fn js_round(num: f64) -> f64 {
    let x = num.floor();
    if num - x >= 0.5 {
        num.ceil()
    } else {
        x
    }
}

/// 对齐 Dart `(value * 100).round() / 100.0`（半数远离零）。
fn round_to_2(value: f64) -> f64 {
    let scaled = value * 100.0;
    let rounded = if scaled >= 0.0 {
        (scaled + 0.5).floor()
    } else {
        (scaled - 0.5).ceil()
    };
    rounded / 100.0
}

/// 奇数为 -1.0、偶数为 0.0（对齐 Python 的 `is_odd`）。
fn is_odd(num: usize) -> f64 {
    if num % 2 != 0 {
        -1.0
    } else {
        0.0
    }
}

/// 非负浮点数 → 保留小数部分的十六进制字符串（2.75 → "2.C"），对齐 Python 的 `float_to_hex`。
fn float_to_hex(x: f64) -> String {
    let mut result = String::new();
    let fraction = x - (x as i64) as f64;
    let mut quotient = x as i64;
    let mut current = x;

    while quotient > 0 {
        quotient = (current / 16.0) as i64;
        let remainder = (current - (quotient as f64) * 16.0) as i64;
        let c = if remainder > 9 {
            (b'A' + (remainder - 10) as u8) as char
        } else {
            (b'0' + remainder as u8) as char
        };
        result.insert(0, c);
        current = quotient as f64;
    }

    if fraction == 0.0 {
        return result;
    }

    result.push('.');
    let mut frac = fraction;
    while frac > 0.0 {
        frac *= 16.0;
        let integer = frac as i64;
        frac -= integer as f64;
        let c = if integer > 9 {
            (b'A' + (integer - 10) as u8) as char
        } else {
            (b'0' + integer as u8) as char
        };
        result.push(c);
    }
    result
}

fn interpolate(from: &[f64], to: &[f64], f: f64) -> Result<Vec<f64>, String> {
    if from.len() != to.len() {
        return Err(format!(
            "Mismatched interpolation arguments: {from:?} vs {to:?}"
        ));
    }
    Ok(from
        .iter()
        .zip(to.iter())
        .map(|(a, b)| a * (1.0 - f) + b * f)
        .collect())
}

/// 三次贝塞尔求值（对齐 QuaX `cubic_curve.dart`，包括那个 1e-5 的二分收敛判据）。
struct Cubic {
    curves: Vec<f64>,
}

impl Cubic {
    fn new(curves: Vec<f64>) -> Self {
        Self { curves }
    }

    fn value(&self, time: f64) -> Result<f64, String> {
        if self.curves.len() < 4 {
            return Err(format!(
                "贝塞尔曲线的控制点不够（只有 {} 个）",
                self.curves.len()
            ));
        }
        let curves = &self.curves;
        let mut start_gradient = 0.0;
        let mut end_gradient = 0.0;
        let mut mid = 0.0;
        let end = 1.0;

        if time <= 0.0 {
            if curves[0] > 0.0 {
                start_gradient = curves[1] / curves[0];
            } else if curves[1] == 0.0 && curves[2] > 0.0 {
                start_gradient = curves[3] / curves[2];
            }
            return Ok(start_gradient * time);
        }
        if time >= end {
            if curves[2] < 1.0 {
                end_gradient = (curves[3] - 1.0) / (curves[2] - 1.0);
            } else if curves[2] == 1.0 && curves[0] < 1.0 {
                end_gradient = (curves[1] - 1.0) / (curves[0] - 1.0);
            }
            return Ok(1.0 + end_gradient * (time - 1.0));
        }

        let mut lo = 0.0;
        let mut hi = end;
        while lo < hi {
            mid = (lo + hi) / 2.0;
            let x_est = calculate(curves[0], curves[2], mid);
            if (time - x_est).abs() < 0.00001 {
                return Ok(calculate(curves[1], curves[3], mid));
            }
            if x_est < time {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        Ok(calculate(curves[1], curves[3], mid))
    }
}

fn calculate(a: f64, b: f64, m: f64) -> f64 {
    3.0 * a * (1.0 - m) * (1.0 - m) * m + 3.0 * b * (1.0 - m) * m * m + m * m * m
}

/// 旋转角（度）→ 2×2 矩阵 [cos, -sin, sin, cos]。
fn convert_rotation_to_matrix(rotation: f64) -> Vec<f64> {
    let rad = rotation * std::f64::consts::PI / 180.0;
    vec![rad.cos(), -rad.sin(), rad.sin(), rad.cos()]
}

// ===========================================================================
// 极小的 HTML 扫描（只为了抠上面两样东西）
// ===========================================================================

struct Tag {
    name: String,
    attrs: Vec<(String, String)>,
}

impl Tag {
    fn attr(&self, name: &str) -> Option<String> {
        self.attrs
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.clone())
    }
}

/// 按出现顺序扫出所有开始标签（够用：只认属性，不建树）。
fn tags(html: &str) -> Vec<Tag> {
    let bytes = html.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'<' {
            i += 1;
            continue;
        }
        // 注释 / 结束标签 / CDATA 跳过
        if html[i..].starts_with("<!--") {
            match html[i + 4..].find("-->") {
                Some(end) => {
                    i += 4 + end + 3;
                    continue;
                }
                None => break,
            }
        }
        if i + 1 < bytes.len() && (bytes[i + 1] == b'/' || bytes[i + 1] == b'!' || bytes[i + 1] == b'?')
        {
            i += 1;
            continue;
        }
        let (tag_text, next) = match read_tag(html, i) {
            Some(v) => v,
            None => break,
        };
        if let Some(tag) = parse_tag(tag_text) {
            out.push(tag);
        }
        i = next;
    }
    out
}

/// 从 `<` 读到配对 `>`（跳过引号里的 `>`），返回（标签正文, 下一个位置）。
fn read_tag(html: &str, start: usize) -> Option<(&str, usize)> {
    let bytes = html.as_bytes();
    let mut i = start + 1;
    let mut quote: Option<u8> = None;
    while i < bytes.len() {
        let b = bytes[i];
        match quote {
            Some(q) => {
                if b == q {
                    quote = None;
                }
            }
            None => match b {
                b'"' | b'\'' => quote = Some(b),
                b'>' => return Some((&html[start + 1..i], i + 1)),
                _ => {}
            },
        }
        i += 1;
    }
    None
}

fn parse_tag(text: &str) -> Option<Tag> {
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() && (bytes[i] as char).is_whitespace() {
        i += 1;
    }
    let name_start = i;
    while i < bytes.len() && !(bytes[i] as char).is_whitespace() && bytes[i] != b'/' {
        i += 1;
    }
    if i == name_start {
        return None;
    }
    let name = text[name_start..i].to_string();
    let mut attrs = Vec::new();
    while i < bytes.len() {
        while i < bytes.len() && (bytes[i] as char).is_whitespace() {
            i += 1;
        }
        if i >= bytes.len() || bytes[i] == b'/' {
            break;
        }
        let key_start = i;
        while i < bytes.len()
            && !(bytes[i] as char).is_whitespace()
            && bytes[i] != b'='
            && bytes[i] != b'/'
        {
            i += 1;
        }
        let key = text[key_start..i].to_string();
        while i < bytes.len() && (bytes[i] as char).is_whitespace() {
            i += 1;
        }
        if i < bytes.len() && bytes[i] == b'=' {
            i += 1;
            while i < bytes.len() && (bytes[i] as char).is_whitespace() {
                i += 1;
            }
            let value = if i < bytes.len() && (bytes[i] == b'"' || bytes[i] == b'\'') {
                let q = bytes[i];
                i += 1;
                let value_start = i;
                while i < bytes.len() && bytes[i] != q {
                    i += 1;
                }
                let value = html_unescape(&text[value_start..i]);
                if i < bytes.len() {
                    i += 1;
                }
                value
            } else {
                let value_start = i;
                while i < bytes.len() && !(bytes[i] as char).is_whitespace() && bytes[i] != b'>' {
                    i += 1;
                }
                html_unescape(&text[value_start..i])
            };
            if !key.is_empty() {
                attrs.push((key, value));
            }
        } else if !key.is_empty() {
            attrs.push((key, String::new()));
        }
    }
    Some(Tag { name, attrs })
}

/// 只处理属性值里最常见的那几个实体（meta 的 content 是 base64，用不到）。
fn html_unescape(text: &str) -> String {
    if !text.contains('&') {
        return text.to_string();
    }
    text.replace("&amp;", "&")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
}

/// 一帧动画 SVG：它内部按出现顺序的 `<path d="…">`。
struct Frame {
    paths: Vec<String>,
}

/// 页面里所有 `id` 以 `loading-x-anim` 开头的元素 → 各自内部的 path 列表（文档顺序）。
fn frame_elements(html: &str) -> Vec<Frame> {
    let all = tags(html);
    let mut out = Vec::new();
    for (index, tag) in all.iter().enumerate() {
        let Some(id) = tag.attr("id") else { continue };
        if !id.starts_with("loading-x-anim") {
            continue;
        }
        // 这一帧的整段（从本标签结束到 `</同名>`）里找 path
        let Some(own_start) = tag_start(html, index) else {
            continue;
        };
        let own_end = element_end(html, own_start, &tag.name).unwrap_or(html.len());
        let slice = &html[own_start.min(html.len())..own_end.min(html.len()).max(own_start)];
        let mut paths = Vec::new();
        for inner in tags(slice) {
            if inner.name.eq_ignore_ascii_case("path") {
                if let Some(d) = inner.attr("d") {
                    paths.push(d);
                }
            }
        }
        out.push(Frame { paths });
    }
    out
}

/// 第 `index` 个标签在原文里的起始偏移（重新扫一遍；标签不多，无所谓）。
fn tag_start(html: &str, index: usize) -> Option<usize> {
    let mut seen = 0;
    let mut i = 0;
    let bytes = html.as_bytes();
    while i < bytes.len() {
        if bytes[i] != b'<' {
            i += 1;
            continue;
        }
        if html[i..].starts_with("<!--") {
            match html[i + 4..].find("-->") {
                Some(end) => {
                    i += 4 + end + 3;
                    continue;
                }
                None => return None,
            }
        }
        if i + 1 < bytes.len() && (bytes[i + 1] == b'/' || bytes[i + 1] == b'!' || bytes[i + 1] == b'?')
        {
            i += 1;
            continue;
        }
        let (_, next) = read_tag(html, i)?;
        if seen == index {
            return Some(i);
        }
        seen += 1;
        i = next;
    }
    None
}

/// 从 `start`（`<name …>` 的起点）找配对的结束标签位置（不含）。
fn element_end(html: &str, start: usize, name: &str) -> Option<usize> {
    let close = format!("</{}", name.to_ascii_lowercase());
    // 只在**标签名**上做 ASCII 大小写不敏感：整页 `to_lowercase()` 既慢（每个字符查 Unicode 表）
    // 又会把那张表拉进 .so，而我们要找的都是 ASCII 标签。
    let lower = html.to_ascii_lowercase();
    lower[start..]
        .find(&close)
        .map(|offset| start + offset)
        .or_else(|| {
            // 自闭合 / 没有结束标签：退回到下一个 `<svg` 之前
            let next = lower[start + 1..].find("</svg")?;
            Some(start + 1 + next)
        })
}

// ===========================================================================
// SHA-256 / Base64
// ===========================================================================

/// SHA-256。txid 的哈希输入只有几十字节。
pub fn sha256(data: &[u8]) -> [u8; 32] {
    use sha2::Digest;
    sha2::Sha256::digest(data).into()
}

/// 十六进制小写（测试 / 诊断用；SHA-256 的已知向量要能直接看出来）。
pub fn hex_of(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

const B64_ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// 标准 base64（**不带 padding**，对齐 `Base64.getEncoder().withoutPadding()`）。
pub fn base64_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity((data.len() + 2) / 3 * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(B64_ALPHABET[(n >> 18) as usize & 0x3f] as char);
        out.push(B64_ALPHABET[(n >> 12) as usize & 0x3f] as char);
        if chunk.len() > 1 {
            out.push(B64_ALPHABET[(n >> 6) as usize & 0x3f] as char);
        }
        if chunk.len() > 2 {
            out.push(B64_ALPHABET[n as usize & 0x3f] as char);
        }
    }
    out
}

/// 标准 base64 解码（对齐 `Base64.getDecoder()`：允许带 `=` 的 padding，其它字符当错误）。
pub fn base64_decode(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() / 4 * 3);
    let mut acc: u32 = 0;
    let mut bits = 0;
    let mut padding = 0;
    for c in text.bytes() {
        if c == b'=' {
            padding += 1;
            continue;
        }
        if c.is_ascii_whitespace() {
            continue;
        }
        let value = B64_ALPHABET.iter().position(|&b| b == c)? as u32;
        acc = (acc << 6) | value;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(((acc >> bits) & 0xFF) as u8);
        }
    }
    if padding > 2 || bits >= 6 {
        return None;
    }
    Some(out)
}
