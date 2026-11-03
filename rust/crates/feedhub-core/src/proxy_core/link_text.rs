//! `core/proxy/LinkText.kt` 的 Rust 版：百分号解码、base64、按行取条目、拆分享链接。
//!
//! 口径逐条照抄 Kotlin：
//!
//! - **不用 URL 解析库**：机场给的链接里 uuid / 密码 / 备注名常常带没转义的 `%`、`/`、中文，
//!   `java.net.URI` 会直接抛异常；Kotlin 自己按 `://`、`@`、`?`、`#` 拆更耐操，这里照搬。
//!   （`url` crate 也不合适：它会规范化、会拒绝坏字符，和"原样拆"是两回事。）
//! - **百分号解码不解 `+`**：base64 里的 `+` 不能变成空格。
//! - **base64 认标准 / URL-safe、有无 padding**，解出来是控制字符就当"不是 base64"。
//! - 缺省端口按协议补：vless / trojan / hysteria2 / tuic 是 443，ss 是 8388。

/// 百分号解码；不解 `+`。解码失败（`%` 后面不是两位十六进制）就原样留下那几个字节。
///
/// 和 Kotlin 一样按**字节**拼、最后整体按 UTF-8 解；Kotlin 那边 `ByteArray.toString(Charsets.UTF_8)`
/// 遇到坏字节会换成 U+FFFD，这里用 `String::from_utf8_lossy` 对齐。
pub fn percent_decode(s: &str) -> String {
    if !s.contains('%') {
        return s.to_string();
    }
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0usize;
    while i < bytes.len() {
        let c = s[i..].chars().next().unwrap_or('\u{fffd}');
        // Kotlin 的判定是 `c == '%' && i + 2 < s.length`（注意是 `<`，所以最后两位 `%41` 不认）
        if c == '%' && i + 2 < s.len() {
            let hex = &s[i + 1..i + 3];
            match u8::from_str_radix(hex, 16) {
                Ok(v) => {
                    out.push(v);
                    i += 3;
                    continue;
                }
                Err(_) => {
                    out.extend_from_slice(c.to_string().as_bytes());
                    i += c.len_utf8();
                    continue;
                }
            }
        }
        out.extend_from_slice(c.to_string().as_bytes());
        i += c.len_utf8();
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// base64（标准 / URL-safe、有无 padding 都认）。不是 base64 就返回 `None`。
///
/// Kotlin 的判定顺序：先过一遍 `[A-Za-z0-9+\-_/]*={0,2}`，再按 `%4` 补 padding，
/// 先用标准解码器、失败再试 URL 解码器；解出来有 `code < 9` 的字符就当"不是文本"。
pub fn base64_decode(text: &str) -> Option<String> {
    let t: String = text
        .trim()
        .chars()
        .filter(|c| *c != '\n' && *c != '\r' && *c != ' ')
        .collect();
    if t.is_empty() {
        return None;
    }
    if !looks_like_base64_body(&t) {
        return None;
    }
    let padded = match t.len() % 4 {
        0 => t.clone(),
        2 => format!("{t}=="),
        3 => format!("{t}="),
        _ => return None,
    };
    let bytes = decode_standard(&padded).or_else(|| decode_url(&padded))?;
    let s = String::from_utf8_lossy(&bytes).into_owned();
    if s.chars().any(|c| (c as u32) < 9) {
        return None;
    }
    Some(s)
}

/// `[A-Za-z0-9+\-_/]*={0,2}`（整串匹配）。
fn looks_like_base64_body(t: &str) -> bool {
    let bytes = t.as_bytes();
    let mut body = bytes.len();
    let mut eq = 0;
    while body > 0 && bytes[body - 1] == b'=' {
        body -= 1;
        eq += 1;
    }
    if eq > 2 {
        return false;
    }
    bytes[..body].iter().all(|b| {
        b.is_ascii_alphanumeric() || matches!(*b, b'+' | b'-' | b'_' | b'/')
    })
}

/// 标准 base64（`+` `/`）。JDK 的 `Base64.getDecoder()` 只认带 `=` 补齐的整 4 元组。
fn decode_standard(s: &str) -> Option<Vec<u8>> {
    base64::Engine::decode(&base64::engine::general_purpose::STANDARD, s).ok()
}

/// URL-safe base64（`-` `_`）。JDK 的 `getUrlDecoder()` **也认** `+` `/`（这是个怪癖，`b64.rs` 里也照着做了）。
fn decode_url(s: &str) -> Option<Vec<u8>> {
    base64::Engine::decode(&base64::engine::general_purpose::URL_SAFE, s).ok()
}

/// 大致像不像 base64 文本（订阅整体是 base64 时用它判断要不要解码）。
pub fn looks_like_base64(text: &str) -> bool {
    let t = text.trim();
    if t.is_empty() || t.contains("://") {
        return false;
    }
    let count = t.chars().count();
    if count < 16 {
        return false;
    }
    // **必须是 ASCII**：Kotlin 的正则是 `[A-Za-z0-9+\-_/=\s]{16,}`，中文 / emoji 都不算
    t.chars().all(|c| {
        c.is_ascii_alphanumeric()
            || matches!(c, '+' | '-' | '_' | '/' | '=')
            || c.is_whitespace()
    })
}

/// 按行拆，去掉空行和 `#` / `//` 注释行。返回 `(行号, 内容)`，行号从 1 数（报"跳过第几条"用）。
///
/// **行号和 Kotlin 的 `lineSequence()` 一样按 `\n` 数**：`\r\n` 的 `\r` 归到内容里、
/// 之后 `trim()` 掉；末尾多一个换行时 `lineSequence()` 不会多产生一行。
pub fn non_empty_lines(text: &str) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    let mut lines: Vec<&str> = text.split('\n').collect();
    // Kotlin 的 `lineSequence()` 不把"末尾换行之后的空串"当一行
    if text.ends_with('\n') {
        lines.pop();
    }
    if text.is_empty() {
        lines.clear();
    }
    for (i, line) in lines.iter().enumerate() {
        let t = crate::hosted::kotlin_trim(line);
        if t.is_empty() || t.starts_with('#') || t.starts_with("//") {
            continue;
        }
        out.push((i + 1, t.to_string()));
    }
    out
}

/// 拆出来的链接。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkParts {
    pub scheme: String,
    pub user_info: String,
    pub host: String,
    pub port: i32,
    pub path: String,
    /// **保持出现顺序**的查询参数（Kotlin 是 `LinkedHashMap`；同名后者覆盖前者但**位置留在第一次**）。
    pub query: Vec<(String, String)>,
    pub fragment: String,
}

impl LinkParts {
    /// 查询参数里取字符串（按名字列表找第一个非空白的）。
    pub fn str_param(&self, names: &[&str]) -> Option<String> {
        for name in names {
            if let Some(v) = self.query.iter().find(|(k, _)| k == name).map(|(_, v)| v) {
                if !crate::proxy_core::is_blank(v) {
                    return Some(v.clone());
                }
            }
        }
        None
    }

    /// 查询参数里取布尔：`1` / `true` / 空值都算 true。
    pub fn bool_param(&self, names: &[&str]) -> bool {
        for name in names {
            if let Some(v) = self.query.iter().find(|(k, _)| k == name).map(|(_, v)| v) {
                return v.is_empty() || v == "1" || v.eq_ignore_ascii_case("true");
            }
        }
        false
    }
}

/// 拆一条分享链接。格式不对（没有 `://`、没有主机、端口不是数字）返回 `None`。
pub fn parse_link(raw: &str) -> Option<LinkParts> {
    let text = crate::hosted::kotlin_trim(raw);
    let sep = text.find("://")?;
    if sep == 0 {
        return None;
    }
    let scheme = text[..sep].to_lowercase();
    let mut rest = &text[sep + 3..];

    let (fragment, tail) = match rest.find('#') {
        Some(hash) => (percent_decode(&rest[hash + 1..]), &rest[..hash]),
        None => (String::new(), rest),
    };
    rest = tail;

    let mut query: Vec<(String, String)> = Vec::new();
    if let Some(q) = rest.find('?') {
        for part in rest[q + 1..].split('&') {
            if part.trim().is_empty() {
                continue;
            }
            let (k, v) = match part.find('=') {
                Some(eq) => (&part[..eq], &part[eq + 1..]),
                None => (part, ""),
            };
            let key = percent_decode(k);
            let value = percent_decode(v);
            // Kotlin 的 `.toMap()`：同名的后一次覆盖，但**位置留在第一次出现处**
            match query.iter_mut().find(|(existing, _)| *existing == key) {
                Some(slot) => slot.1 = value,
                None => query.push((key, value)),
            }
        }
        rest = &rest[..q];
    }

    let (path, authority) = match rest.find('/') {
        Some(slash) => (rest[slash..].to_string(), &rest[..slash]),
        None => (String::new(), rest),
    };

    let (user_info, host_port) = match authority.rfind('@') {
        Some(at) => (&authority[..at], &authority[at + 1..]),
        None => ("", authority),
    };
    if host_port.trim().is_empty() {
        return None;
    }

    let (host, port_text) = if host_port.starts_with('[') {
        // `[::1]:8443` / `[::1]`：Kotlin 的 `host` **带着方括号**（`substring(0, end + 1)`）
        let end = host_port.find(']')?;
        let host = host_port[..end + 1].to_string();
        let after = &host_port[end + 1..];
        (host, after.strip_prefix(':').unwrap_or(after).to_string())
    } else {
        match host_port.rfind(':') {
            Some(colon) => (
                host_port[..colon].to_string(),
                host_port[colon + 1..].to_string(),
            ),
            None => (host_port.to_string(), String::new()),
        }
    };
    if host.trim().is_empty() {
        return None;
    }
    let default_port = match scheme.as_str() {
        "vless" | "trojan" | "hysteria2" | "hy2" | "tuic" => 443,
        "ss" => 8388,
        _ => return None,
    };
    let port: i32 = if port_text.trim().is_empty() {
        default_port
    } else {
        match port_text.parse::<i32>() {
            Ok(p) => p,
            Err(_) => return None,
        }
    };
    if !(1..=65535).contains(&port) {
        return None;
    }
    Some(LinkParts {
        scheme,
        user_info: percent_decode(user_info),
        host,
        port,
        path,
        query,
        fragment,
    })
}

