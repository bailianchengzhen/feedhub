//! **Cookie 罐**：`core/Cookies.kt` + `core/CookieStore.kt` 的 Rust 版。
//!
//! 罐子的**全部逻辑**在这里：域名 / 路径匹配（OkHttp `Cookie.matches` 的口径）、过期、hostOnly / secure、
//! 去重键 `(name, domain, path)`、读改写合并、`cookies.json` 的编解码、留底（quarantine）、
//! WebView 的 `Cookie: a=1; b=2` 串 → 罐条目的转换。
//!
//! **留在宿主的只有薄适配**：OkHttp 的 `CookieJar.loadForRequest` / `saveFromResponse` 把 OkHttp `Cookie`
//! 转成本模块的 [`StoredCookie`]，再问 [`CookieJar`]；落盘由宿主给（桌面 600 + 原子替换 + 跨进程文件锁，
//! 安卓私有目录）。见 `rust-bridge/.../feedhub/cookies/RustCookieJar.kt`。
//!
//! ## 三条必须逐字节保住的格式
//!
//! 1. **`cookies.json`**（[`CookieJson`]）：kotlinx.serialization `Json { prettyPrint = true;
//!    ignoreUnknownKeys = true }` 写出来的那份。4 空格缩进、`": "` 分隔、字段**声明顺序**
//!    （`name` / `value` / `domain` / `path` / `expiresAt` / `secure` / `httpOnly` / `hostOnly`）、
//!    **默认值不写**（`path="/"`、`false`）、控制字符写成 `\b` `\t` `\n` `\f` `\r` 或小写 `\u00XX`。
//!    「Kotlin 写的 Rust 读、Rust 写的 Kotlin 读，双向逐字节」由 `CookieJsonByteTest` 钉着。
//! 2. **合并的语义**（[`CookieJar`]）：写回是"以磁盘那一份为底、叠上这份罐子自己的改动"，不是整份覆盖。
//!    桌面 App 和 `feedhub` cli 是两个进程，整份覆盖会把对方刚存下的登录态 / 验证码 `validate` 盖掉。
//!    `dirty` / `removed` / `cleared_domains` 三样都**只影响接下来那一次**写回。
//! 3. **读不懂的存档改名留底**（不是悄悄当空罐）：留不了底就抛错，宁可起不来也不覆盖用户的登录态。
//!
//! ## 线程模型
//!
//! 罐子会被**多个 OkHttp 线程同时调**（连接池里的每个请求线程都会 `loadForRequest`），所以内部是
//! `Mutex<Inner>`；落盘那一步（读磁盘 → 合并 → 写磁盘）一直在同一把锁里做，和 Kotlin 的
//! `@Synchronized` + `file.locked {}` 一样。`&self` 方法全部不加 `&mut`：JNA 那边是一个共享的
//! `Arc<CookieJar>`。

use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::time::{Duration, Instant};

/// 一条 Cookie（Kotlin `okhttp3.Cookie` 的那几个字段）。
///
/// `domain` **不带前导点**（OkHttp 的 `Cookie.domain` 就是这样：`.bilibili.com` 存成 `bilibili.com`
/// 再加 `host_only = false`）。`expires_at_ms` 是 Unix 毫秒；**≤ 现在就是过期**（服务端删 Cookie 也用这个表达）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredCookie {
    pub name: String,
    pub value: String,
    pub domain: String,
    pub path: String,
    pub expires_at_ms: i64,
    pub secure: bool,
    pub http_only: bool,
    pub host_only: bool,
}

impl StoredCookie {
    /// 去重键 `(name, domain, path)`（Kotlin `PersistentCookieJar.CookieKey`）。
    pub fn key(&self) -> CookieKey {
        CookieKey {
            name: self.name.clone(),
            domain: self.domain.clone(),
            path: self.path.clone(),
        }
    }

    /// OkHttp `Cookie.matches(url)` 的逐行移植：
    /// ① 域名匹配（hostOnly 时要求**完全相等**，否则走 [`domain_match`]）；
    /// ② 路径匹配（[`path_match`]）；③ `secure` 的 Cookie 只发给 https。
    ///
    /// 注意 ③ 是 `secure && !url.is_https()` → 不匹配：`secure` 的 Cookie 在 http 上**不发**，
    /// 非 secure 的在 https 上也照发（浏览器口径）。
    pub fn matches(&self, url: &HttpUrl) -> bool {
        let domain_ok = if self.host_only {
            url.host == self.domain
        } else {
            domain_match(&url.host, &self.domain)
        };
        if !domain_ok {
            return false;
        }
        if !path_match(&url.encoded_path, &self.path) {
            return false;
        }
        !(self.secure && !url.is_https)
    }

    /// `true` = 已经过期（Kotlin 的判定一律是 `expiresAt <= now`）。
    pub fn expired(&self, now_ms: i64) -> bool {
        self.expires_at_ms <= now_ms
    }
}

/// 罐子的去重键：**同名同域同路径**才算同一条（Kotlin `CookieKey`）。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CookieKey {
    pub name: String,
    pub domain: String,
    pub path: String,
}

/// 一个"请求地址"里罐子要用到的三样：host（已规范化）、编码后的 path、是不是 https。
///
/// 由宿主（OkHttp 的 `HttpUrl`）算好传进来——`HttpUrl` 的 host 规范化（IDN → punycode、小写、
/// IPv6 去掉方括号）和编码后的 path 都在 OkHttp 里，重写一遍就等于把线上行为改掉。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpUrl {
    pub host: String,
    pub encoded_path: String,
    pub is_https: bool,
}

impl HttpUrl {
    /// 从 `scheme://host/path` 这种最小信息拼一个（宿主和测试都用它）。
    pub fn new(host: impl Into<String>, encoded_path: impl Into<String>, is_https: bool) -> Self {
        Self {
            host: host.into(),
            encoded_path: encoded_path.into(),
            is_https,
        }
    }
}

/// OkHttp `Cookie.Companion.domainMatch`：完全相同，或者"以 `.domain` 结尾且前缀不是 IP"。
///
/// `string.endsWith(suffix)` + `string[string.length - suffix.length - 1] == '.'` +
/// `!canParseAsIpAddress(string)`。第三步是那个正则 `([0-9a-fA-F]*:[0-9a-fA-F:.]*)|([\d.]+)`
/// 的完整匹配（见 [`can_parse_as_ip_address`]）。
pub fn domain_match(url_host: &str, domain: &str) -> bool {
    if url_host == domain {
        return true;
    }
    if !url_host.ends_with(domain) {
        return false;
    }
    // 字符下标要按字节切：OkHttp 在这里用的是 `String.charAt`（UTF-16 下标）比 '.'，
    // 而 '.' 是 ASCII，只有"往前一个字符"落在多字节字符的中间才会出错。ASCII 之外的字符
    // 不可能让 `charAt(i)` 等于 '.'，所以按字节判 `as_bytes()[i] == b'.'` 等价。
    let bytes = url_host.as_bytes();
    let idx = bytes.len() as i64 - domain.len() as i64 - 1;
    if idx < 0 || idx as usize >= bytes.len() || bytes[idx as usize] != b'.' {
        return false;
    }
    !can_parse_as_ip_address(url_host)
}

/// OkHttp `Util.canParseAsIpAddress`：正则 `([0-9a-fA-F]*:[0-9a-fA-F:.]*)|([\d.]+)` 的**完整匹配**。
///
/// 两条分支：
/// 1. `[0-9a-fA-F]*:[0-9a-fA-F:.]*`：任意个十六进制字符 + 一个冒号 + 十六进制 / 冒号 / 点（IPv6 的宽松形状）；
/// 2. `[\d.]+`：只有数字和点（IPv4 的宽松形状，`1.2.3.4`、`999.999` 都算）。
///
/// 注意它是**宽松**的：`aaaa:bb` 也算 IP（OkHttp 原样如此），别"修正"成严格解析——`domain_match`
/// 的语义会跟着变（`evil.com.1.2.3.4` 这类域会被误判成 IP 而**允许**子域匹配）。
pub fn can_parse_as_ip_address(host: &str) -> bool {
    if host.is_empty() {
        return false;
    }
    // 分支 2：[0-9.]+
    if host.bytes().all(|b| b.is_ascii_digit() || b == b'.') {
        return true;
    }
    // 分支 1：[0-9a-fA-F]* ':' [0-9a-fA-F:.]*
    let Some(colon) = host.find(':') else {
        return false;
    };
    let (head, tail) = host.split_at(colon);
    let tail = &tail[1..];
    head.bytes().all(|b| b.is_ascii_hexdigit())
        && tail
            .bytes()
            .all(|b| b.is_ascii_hexdigit() || b == b':' || b == b'.')
}

/// OkHttp `Cookie.Companion.pathMatch`：
/// - 路径完全相同 → 匹配；
/// - Cookie 的 path 是 `/` → 匹配一切；
/// - 请求路径以 Cookie 的 path 开头，且 Cookie 的 path 以 `/` 结尾 → 匹配；
/// - 请求路径以 Cookie 的 path 开头，且它的**下一个字符**是 `/` → 匹配（`/foo` 匹配 `/foo/bar` 但不匹配 `/foobar`）。
pub fn path_match(encoded_path: &str, cookie_path: &str) -> bool {
    if encoded_path == cookie_path {
        return true;
    }
    if encoded_path.starts_with(cookie_path) {
        if cookie_path.ends_with('/') {
            return true;
        }
        // OkHttp：`urlPath[cookiePath.length] == '/'`（下标按 char；'/' 是 ASCII，按字节等价）
        if let Some(b) = encoded_path.as_bytes().get(cookie_path.len()) {
            if *b == b'/' {
                return true;
            }
        }
    }
    false
}

// ===========================================================================
// cookies.json 的编解码
// ===========================================================================

/// `cookies.json` 的编解码（Kotlin `object CookieJson`）。
///
/// 编码口径照 kotlinx.serialization 的 `prettyPrint = true`：
/// ```text
/// [
///     {
///         "name": "SESSDATA",
///         "value": "…",
///         "domain": "bilibili.com",
///         "expiresAt": 1700000000000,
///         "secure": true
///     },
///     { … }
/// ]
/// ```
/// 空表是 `[]`（**两行**：`[]` 之后没有换行——`Json.encodeToString` 不给结尾换行）。
///
/// 字段顺序 = `StoredCookie` 的声明顺序；默认值（`path="/"` / `false`）**不写**；`expiresAt` 一定写
/// （没有默认值）。
pub mod cookie_json {
    use super::StoredCookie;

    /// 编码成 `cookies.json` 的那份文本（**逐字节**等于 Kotlin `CookieJson.encode`）。
    pub fn encode(cookies: &[StoredCookie]) -> String {
        if cookies.is_empty() {
            return "[]".to_string();
        }
        let mut out = String::with_capacity(64 + cookies.len() * 160);
        out.push('[');
        for (i, c) in cookies.iter().enumerate() {
            if i == 0 {
                out.push('\n');
            } else {
                out.push_str(",\n");
            }
            out.push_str("    {\n");
            out.push_str("        \"name\": ");
            push_json_string(&mut out, &c.name);
            out.push_str(",\n        \"value\": ");
            push_json_string(&mut out, &c.value);
            out.push_str(",\n        \"domain\": ");
            push_json_string(&mut out, &c.domain);
            // path 的默认值是 "/"：等于默认值就不写（kotlinx 的 encodeDefaults = false 是默认行为）
            if c.path != "/" {
                out.push_str(",\n        \"path\": ");
                push_json_string(&mut out, &c.path);
            }
            out.push_str(",\n        \"expiresAt\": ");
            out.push_str(&c.expires_at_ms.to_string());
            // 三个 bool 的默认值都是 false
            if c.secure {
                out.push_str(",\n        \"secure\": true");
            }
            if c.http_only {
                out.push_str(",\n        \"httpOnly\": true");
            }
            if c.host_only {
                out.push_str(",\n        \"hostOnly\": true");
            }
            out.push_str("\n    }");
        }
        out.push_str("\n]");
        out
    }

    /// kotlinx.serialization 的字符串转义：`"` `\` `\b` `\t` `\n` `\f` `\r`，别的 `< 0x20` 写成
    /// **小写** `\u00xx`；`>= 0x20` 原样（含 U+2028 / U+2029 / 非 BOM 的宽字符——kotlinx 不转义它们）。
    fn push_json_string(out: &mut String, s: &str) {
        out.push('"');
        for c in s.chars() {
            match c {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                '\u{8}' => out.push_str("\\b"),
                '\u{c}' => out.push_str("\\f"),
                c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
                c => out.push(c),
            }
        }
        out.push('"');
    }

    /// 解码结果。
    #[derive(Debug)]
    pub enum DecodeError {
        /// 不是 JSON / 形状不对（顶层不是数组、元素不是对象、`expiresAt` 缺失或不是数字…）。
        Malformed { detail: String },
    }

    impl std::fmt::Display for DecodeError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            match self {
                Self::Malformed { detail } => write!(f, "{detail}"),
            }
        }
    }

    /// 解码 `cookies.json`。**未知字段忽略**（`ignoreUnknownKeys = true`）；缺省字段用默认值
    /// （`path="/"`、三个 bool = false）。
    ///
    /// `expiresAt` 缺失 / 是 null / 不是整数时**报错**：那是"读不懂的存档"，调用方会走留底
    /// （和 Kotlin 那边 `MissingFieldException` 一样，只是 Kotlin 的异常在 `PersistentCookieJar.init` 里
    /// 被 catch 住走留底）。
    pub fn decode(text: &str) -> Result<Vec<StoredCookie>, DecodeError> {
        let value: serde_json::Value = serde_json::from_str(text).map_err(|e| DecodeError::Malformed {
            detail: format!("不是合法 JSON：{e}"),
        })?;
        let arr = match value {
            serde_json::Value::Array(a) => a,
            other => {
                return Err(DecodeError::Malformed {
                    detail: format!("顶层不是数组（是 {}）", kind_of(&other)),
                })
            }
        };
        let mut out = Vec::with_capacity(arr.len());
        for (i, item) in arr.iter().enumerate() {
            let obj = match item {
                serde_json::Value::Object(o) => o,
                other => {
                    return Err(DecodeError::Malformed {
                        detail: format!("第 {i} 项不是对象（是 {}）", kind_of(other)),
                    })
                }
            };
            let name = string_field(obj, "name", i)?.ok_or_else(|| DecodeError::Malformed {
                detail: format!("第 {i} 项缺 name"),
            })?;
            let value = string_field(obj, "value", i)?.ok_or_else(|| DecodeError::Malformed {
                detail: format!("第 {i} 项缺 value"),
            })?;
            let domain = string_field(obj, "domain", i)?.ok_or_else(|| DecodeError::Malformed {
                detail: format!("第 {i} 项缺 domain"),
            })?;
            let expires_at_ms = match obj.get("expiresAt") {
                Some(serde_json::Value::Number(n)) => {
                    n.as_i64().ok_or_else(|| DecodeError::Malformed {
                        detail: format!("第 {i} 项的 expiresAt 不是整数"),
                    })?
                }
                Some(serde_json::Value::Null) | None => {
                    return Err(DecodeError::Malformed {
                        detail: format!("第 {i} 项缺 expiresAt"),
                    })
                }
                Some(other) => {
                    return Err(DecodeError::Malformed {
                        detail: format!("第 {i} 项的 expiresAt 不是数字（是 {}）", kind_of(other)),
                    })
                }
            };
            let path = string_field(obj, "path", i)?.unwrap_or_else(|| "/".to_string());
            let secure = bool_field(obj, "secure", i)?.unwrap_or(false);
            let http_only = bool_field(obj, "httpOnly", i)?.unwrap_or(false);
            let host_only = bool_field(obj, "hostOnly", i)?.unwrap_or(false);
            out.push(StoredCookie {
                name,
                value,
                domain,
                path,
                expires_at_ms,
                secure,
                http_only,
                host_only,
            });
        }
        Ok(out)
    }

    fn kind_of(v: &serde_json::Value) -> &'static str {
        match v {
            serde_json::Value::Null => "null",
            serde_json::Value::Bool(_) => "布尔",
            serde_json::Value::Number(_) => "数字",
            serde_json::Value::String(_) => "字符串",
            serde_json::Value::Array(_) => "数组",
            serde_json::Value::Object(_) => "对象",
        }
    }

    fn string_field(
        obj: &serde_json::Map<String, serde_json::Value>,
        key: &str,
        index: usize,
    ) -> Result<Option<String>, DecodeError> {
        match obj.get(key) {
            None | Some(serde_json::Value::Null) => Ok(None),
            Some(serde_json::Value::String(s)) => Ok(Some(s.clone())),
            Some(other) => Err(DecodeError::Malformed {
                detail: format!("第 {index} 项的 {key} 不是字符串（是 {}）", kind_of(other)),
            }),
        }
    }

    fn bool_field(
        obj: &serde_json::Map<String, serde_json::Value>,
        key: &str,
        index: usize,
    ) -> Result<Option<bool>, DecodeError> {
        match obj.get(key) {
            None => Ok(None),
            Some(serde_json::Value::Bool(b)) => Ok(Some(*b)),
            Some(other) => Err(DecodeError::Malformed {
                detail: format!("第 {index} 项的 {key} 不是布尔（是 {}）", kind_of(other)),
            }),
        }
    }
}

pub use cookie_json::DecodeError;

// ===========================================================================
// 落盘端口
// ===========================================================================

/// 落盘（Kotlin `interface CookieFile` 的三样）。
///
/// 实现留在宿主：桌面是 `NioCookieFile`（POSIX 600 + 临时文件 + 原子替换 + `cookies.json.lock`
/// 跨进程文件锁），安卓是 `FileCookieFile`（App 私有目录 + 原子替换，不加锁）。**罐子逻辑不在这里**。
pub trait CookieStorage: Send + Sync {
    /// 读整份文本；文件不存在返回 `None`。
    fn read(&self) -> Option<String>;

    /// 整份覆盖写（实现方保证原子替换）。
    fn write(&self, text: &str) -> Result<(), String>;

    /// 存档读不懂时改名留底（`cookies.json.bad-<毫秒>`），返回留底的路径；做不到返回 `None`。
    fn quarantine(&self) -> Option<String>;

    /// 读改写期间拿着的那把锁（跨进程）。默认不加锁（安卓只有一个进程）。
    ///
    /// 回调里**不能**再调这个罐子的方法（会自己锁自己）；只做"读磁盘 / 写磁盘"。
    fn locked(&self, block: &mut dyn FnMut()) {
        block();
    }

    /// 底层文件指纹 `(dev, ino, mtime_sec, mtime_nsec, size)`。
    ///
    /// 返回 `Some` 且与本实例上次读/写后的指纹一致时，说明没有任何外部进程碰过 `cookies.json`，
    /// `persist_locked` 可以跳过重读与 JSON 反序列化；一旦外部进程（如桌面 CLI）原子换名写过，
    /// `ino` / `mtime` 必然变化，自动回退到跨进程合并。返回 `None`（测试桩等）时始终重读合并。
    fn file_stamp(&self) -> Option<(u64, u64, i64, i64, u64)> {
        None
    }
}

// ===========================================================================
// 罐子
// ===========================================================================

/// 内存里的状态（一把锁护住）。
#[derive(Default)]
struct Inner {
    cookies: Vec<StoredCookie>,
    /// 这份罐子自己写的键：值以内存为准，不被磁盘上的旧值盖回去。
    dirty: HashSet<CookieKey>,
    /// 这份罐子自己删掉的键（过期 Cookie、`remove`）：下次写回时磁盘上的同键也删掉。
    removed: HashSet<CookieKey>,
    /// 这份罐子 `clear_domain` 过的域：下次写回时磁盘上这些域下的也删掉。
    cleared_domains: HashSet<String>,
    /// 上次本罐读/写 `cookies.json` 时的文件指纹。
    last_disk_stamp: Option<(u64, u64, i64, i64, u64)>,
    /// 是否有尚未落盘的内存改动（开防抖时由后台唤醒或 `flush` 消费）。
    pending_persist: bool,
    /// 最近一次写操作的注入时钟（`Drop` 兜底 flush 时用）。
    last_now_ms: i64,
}

#[derive(Default)]
struct DebounceState {
    due_at: Option<Instant>,
    flush_now_ms: i64,
    shutdown: bool,
}

struct DebounceWorker {
    shared: Arc<(Mutex<DebounceState>, Condvar)>,
    join: Option<std::thread::JoinHandle<()>>,
}

/// 存在一份 JSON 文本里的 Cookie 罐（Kotlin `PersistentCookieJar`）。
///
/// 构造时会读磁盘：读不懂的存档**不能悄悄当成空罐**（那样下一次写回会把用户所有平台的登录态覆盖掉），
/// 要先改名留底；留不了底就返回 [`OpenError`]，调用方把错误往上报（App 宁可起不来，也不覆盖）。
///
/// `now_ms` 是注入的时钟（`cookies.json` 里"过期的不往回搬"、`save_from_response` 判过期都要它）；
/// 罐子自己**不读系统时钟**，测试才能一条条对出来（和这一层其余模块的规矩一样）。
pub struct CookieJar {
    /// 落盘端口（`Debug` 手动实现：`Box<dyn>` 不带 `Debug`，测试里 `unwrap_err()` 要它）。
    storage: Box<dyn CookieStorage>,
    inner: Mutex<Inner>,
    /// 防抖窗口（毫秒，0 = 同步立刻落盘；App 设为 2000ms 以合并刷流期间的高频 `Set-Cookie`）。
    debounce_ms: AtomicU64,
    flusher: Mutex<Option<DebounceWorker>>,
}

impl std::fmt::Debug for CookieJar {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CookieJar").field("len", &self.len()).finish()
    }
}

/// 打开罐子失败的原因。
#[derive(Debug)]
pub enum OpenError {
    /// 存档解析失败，而且**没法留底**（Kotlin 抛的那句 `IllegalStateException`）。
    CannotQuarantine { detail: String },
}

impl std::fmt::Display for OpenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CannotQuarantine { detail } => write!(
                f,
                "cookies.json 解析失败，也没法留底；为了不覆盖已有登录，不加载也不写回：{detail}"
            ),
        }
    }
}

impl CookieJar {
    /// 从落盘打开（等于 Kotlin `PersistentCookieJar.init`）。
    ///
    /// 读不懂时：先 `quarantine()` 留底，**留底成功**才从空罐开始（同时往 stderr 打一行，和 Kotlin 一样）；
    /// 留不了底就返回错误。已经过期的条目不加载（`expiresAt <= now`）。
    pub fn open(storage: Box<dyn CookieStorage>, now_ms: i64) -> Result<Self, OpenError> {
        let mut inner = Inner {
            last_now_ms: now_ms,
            ..Inner::default()
        };
        if let Some(text) = storage.read() {
            match cookie_json::decode(&text) {
                Ok(stored) => {
                    inner.cookies = stored.into_iter().filter(|c| !c.expired(now_ms)).collect();
                    inner.last_disk_stamp = storage.file_stamp();
                }
                Err(e) => {
                    let Some(backup) = storage.quarantine() else {
                        return Err(OpenError::CannotQuarantine {
                            detail: e.to_string(),
                        });
                    };
                    eprintln!(
                        "feedhub：cookies.json 解析失败，已改名留底到 {backup}，从空的 Cookie 罐开始"
                    );
                }
            }
        }
        Ok(Self {
            storage,
            inner: Mutex::new(inner),
            debounce_ms: AtomicU64::new(0),
            flusher: Mutex::new(None),
        })
    }

    /// 内存罐子（不落盘；测试 / 演示用）。等于 Kotlin `MemoryCookieJar` 的那一份，只是它也走
    /// 同一套匹配 / 合并逻辑。`storage` 是"什么也不存"的实现。
    pub fn in_memory(now_ms: i64) -> Self {
        Self::open(Box::new(NoStorage), now_ms).expect("内存罐子不会失败")
    }

    /// 开启 / 调整 `save_from_response` 的写盘防抖窗口（毫秒；`0` = 每次改动同步落盘）。
    ///
    /// 后台线程在无脏数据时挂在 `Condvar::wait` 上（**零唤醒**）；有脏数据时合并窗口内的多次写入，
    /// 到点只做一次原子落盘。`onStop` / 退出时调 [`Self::flush`] 立即刷盘。
    pub fn set_debounce_ms(self: &Arc<Self>, ms: u64) {
        self.debounce_ms.store(ms, Ordering::Release);
        if ms == 0 {
            let now = self.inner.lock().unwrap_or_else(|e| e.into_inner()).last_now_ms;
            let _ = self.flush(now);
            return;
        }
        let mut guard = self.flusher.lock().unwrap_or_else(|e| e.into_inner());
        if guard.is_some() {
            return;
        }
        let shared = Arc::new((Mutex::new(DebounceState::default()), Condvar::new()));
        let shared_thread = shared.clone();
        let weak: Weak<CookieJar> = Arc::downgrade(self);
        let join = std::thread::Builder::new()
            .name("feedhub-cookie-flush".into())
            .spawn(move || {
                let (lock, cv) = &*shared_thread;
                let mut st = lock.lock().unwrap_or_else(|e| e.into_inner());
                loop {
                    if st.shutdown {
                        return;
                    }
                    match st.due_at {
                        None => {
                            st = cv.wait(st).unwrap_or_else(|e| e.into_inner());
                        }
                        Some(deadline) => {
                            let now = Instant::now();
                            if now < deadline {
                                let (next_st, _) = cv
                                    .wait_timeout(st, deadline - now)
                                    .unwrap_or_else(|e| e.into_inner());
                                st = next_st;
                                continue;
                            }
                            st.due_at = None;
                            let flush_now_ms = st.flush_now_ms;
                            drop(st);
                            if let Some(jar) = weak.upgrade() {
                                let _ = jar.flush_pending(flush_now_ms);
                            } else {
                                return;
                            }
                            st = lock.lock().unwrap_or_else(|e| e.into_inner());
                        }
                    }
                }
            })
            .ok();
        if join.is_some() {
            *guard = Some(DebounceWorker { shared, join });
        } else {
            // 极端情况下起不了线程就退回同步写盘，绝不丢登录态。
            self.debounce_ms.store(0, Ordering::Release);
        }
    }

    /// 立刻把防抖队列中尚未落盘的改动写回磁盘（没有脏数据时是 O(1) 空操作，不碰磁盘）。
    pub fn flush(&self, now_ms: i64) -> Result<(), StorageError> {
        if let Some(worker) = self.flusher.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            let (lock, _) = &*worker.shared;
            let mut st = lock.lock().unwrap_or_else(|e| e.into_inner());
            st.due_at = None;
        }
        self.flush_pending(now_ms)
    }

    fn flush_pending(&self, now_ms: i64) -> Result<(), StorageError> {
        let need = {
            let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            inner.pending_persist
                || !inner.dirty.is_empty()
                || !inner.removed.is_empty()
                || !inner.cleared_domains.is_empty()
        };
        if !need {
            return Ok(());
        }
        self.persist(now_ms)
    }

    /// 发往 `url` 的请求会带上的 Cookie（顺序 = 罐子里的顺序）。
    pub fn load_for_request(&self, url: &HttpUrl, now_ms: i64) -> Vec<StoredCookie> {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner
            .cookies
            .iter()
            .filter(|c| !c.expired(now_ms) && c.matches(url))
            .cloned()
            .collect()
    }

    /// 发往 `url` 的请求会带上的、名为 `name` 的 Cookie 值（Kotlin `FeedCookieJar.find`）。
    pub fn find(&self, url: &HttpUrl, name: &str, now_ms: i64) -> Option<String> {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner
            .cookies
            .iter()
            .find(|c| !c.expired(now_ms) && c.name == name && c.matches(url))
            .map(|c| c.value.clone())
    }

    /// 服务端下发的 Cookie（或者自己种的）：同名同域同路径的先删，`expiresAt > now` 才留下。
    ///
    /// 若下发的 Cookie 与内存已有有效条目值及属性一致（含长效 `Max-Age` 轻微顺延），不标记 dirty 也不写盘；
    /// 开启防抖时合并延迟落盘，未开启时同步落盘。
    pub fn save_from_response(
        &self,
        cookies: &[StoredCookie],
        now_ms: i64,
    ) -> Result<(), StorageError> {
        if cookies.is_empty() {
            return Ok(());
        }
        let mut changed = false;
        {
            let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            inner.last_now_ms = now_ms;
            for c in cookies {
                let k = c.key();
                let existing_idx = inner
                    .cookies
                    .iter()
                    .position(|x| x.name == c.name && x.domain == c.domain && x.path == c.path);
                if !c.expired(now_ms) {
                    if let Some(idx) = existing_idx {
                        let ex = &inner.cookies[idx];
                        let same_attrs = !ex.expired(now_ms)
                            && ex.value == c.value
                            && ex.secure == c.secure
                            && ex.http_only == c.http_only
                            && ex.host_only == c.host_only;
                        let same_or_sliding_expiry = ex.expires_at_ms == c.expires_at_ms
                            || (ex.expires_at_ms > now_ms.saturating_add(300_000)
                                && c.expires_at_ms > now_ms.saturating_add(300_000)
                                && c.expires_at_ms.saturating_sub(ex.expires_at_ms).saturating_abs()
                                    < 60_000);
                        if same_attrs && same_or_sliding_expiry {
                            inner.cookies[idx].expires_at_ms = c.expires_at_ms;
                            continue;
                        }
                        inner.cookies.remove(idx);
                    }
                    inner.cookies.push(c.clone());
                    inner.dirty.insert(k.clone());
                    inner.removed.remove(&k);
                    changed = true;
                } else if let Some(idx) = existing_idx {
                    // 服务端删已有 Cookie（过期时间）和 `remove`：内存与磁盘同键一起删
                    inner.cookies.remove(idx);
                    inner.dirty.remove(&k);
                    inner.removed.insert(k);
                    changed = true;
                } else if inner.dirty.remove(&k) {
                    inner.removed.insert(k);
                    changed = true;
                }
            }
            if changed {
                inner.pending_persist = true;
            }
        }
        if !changed {
            return Ok(());
        }
        let debounce = self.debounce_ms.load(Ordering::Acquire);
        if debounce > 0 {
            let scheduled = {
                let guard = self.flusher.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(worker) = guard.as_ref() {
                    let (lock, cv) = &*worker.shared;
                    let mut st = lock.lock().unwrap_or_else(|e| e.into_inner());
                    st.flush_now_ms = now_ms;
                    if st.due_at.is_none() {
                        st.due_at = Some(Instant::now() + Duration::from_millis(debounce));
                        cv.notify_one();
                    }
                    true
                } else {
                    false
                }
            };
            if scheduled {
                return Ok(());
            }
        }
        self.persist(now_ms)
    }

    /// 删掉某个域（含子域）下的全部 Cookie。显式删除动作立即落盘。
    pub fn clear_domain(&self, domain: &str, now_ms: i64) -> Result<(), StorageError> {
        let d = domain.trim_start_matches('.');
        {
            let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            inner.last_now_ms = now_ms;
            inner
                .cookies
                .retain(|c| !(c.domain == d || c.domain.ends_with(&format!(".{d}"))));
            inner.cleared_domains.insert(d.to_string());
            inner.pending_persist = true;
        }
        self.flush(now_ms)
    }

    /// 删掉某个域（含子域）下名字在 `names` 里的 Cookie（不管路径），其他 Cookie 不动。
    ///
    /// Kotlin `FeedCookieJar.remove` 的默认实现是"写一条已过期的同名 Cookie"；这里直接走
    /// `save_from_response` + `flush`，显式删除动作立即落盘。
    pub fn remove(&self, domain: &str, names: &[String], now_ms: i64) -> Result<(), StorageError> {
        let d = domain.trim_start_matches('.');
        // 先把要删的挑出来（不能在持有锁时再调 save_from_response）
        let targets: Vec<StoredCookie> = {
            let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            inner
                .cookies
                .iter()
                .filter(|c| names.contains(&c.name))
                .filter(|c| c.domain == d || c.domain.ends_with(&format!(".{d}")))
                .map(|c| StoredCookie {
                    value: String::new(),
                    expires_at_ms: 0,
                    ..c.clone()
                })
                .collect()
        };
        if targets.is_empty() {
            return Ok(());
        }
        self.save_from_response(&targets, now_ms)?;
        self.flush(now_ms)
    }

    /// 全部 Cookie（`cookie list` 用；**不**过滤过期的，和 Kotlin `all()` 一样——过期的会被
    /// `load_for_request` 滤掉、写回时也不往回搬）。
    pub fn all(&self) -> Vec<StoredCookie> {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.cookies.clone()
    }

    /// 现在有几条（`all().len()` 的便宜版，测试 / 排障用）。
    pub fn len(&self) -> usize {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).cookies.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// 写回文件：**以磁盘上那一份为底**，叠上这份罐子自己的改动（见模块头 §2）。
    fn persist(&self, now_ms: i64) -> Result<(), StorageError> {
        let storage = &self.storage;
        let mut result: Result<(), StorageError> = Ok(());
        storage.locked(&mut || {
            result = self.persist_locked(now_ms);
        });
        result
    }

    fn persist_locked(&self, now_ms: i64) -> Result<(), StorageError> {
        let current_stamp = self.storage.file_stamp();
        let disk_unchanged = {
            #[cfg(target_os = "android")]
            {
                current_stamp.is_some()
            }
            #[cfg(not(target_os = "android"))]
            {
                let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
                current_stamp.is_some() && current_stamp == inner.last_disk_stamp
            }
        };
        let disk = if disk_unchanged {
            None
        } else {
            self.read_stored()
        };
        let text = {
            let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            let merged: Vec<StoredCookie> = if disk.is_none() {
                inner
                    .cookies
                    .iter()
                    .filter(|c| !c.expired(now_ms))
                    .cloned()
                    .collect()
            } else {
                let disk = disk.as_ref().unwrap();
                // 用 Vec + 位置索引模拟 LinkedHashMap：键的顺序 = 第一次出现的顺序
                let mut order: Vec<CookieKey> = Vec::new();
                let mut map: std::collections::HashMap<CookieKey, StoredCookie> =
                    std::collections::HashMap::new();
                for c in disk {
                    if c.expired(now_ms) {
                        continue; // 磁盘上留着已过期的条目不往回搬
                    }
                    let k = c.key();
                    if inner.dirty.contains(&k) || inner.removed.contains(&k) {
                        continue;
                    }
                    if inner
                        .cleared_domains
                        .iter()
                        .any(|d| c.domain == *d || c.domain.ends_with(&format!(".{d}")))
                    {
                        continue;
                    }
                    if !map.contains_key(&k) {
                        order.push(k.clone());
                    }
                    map.insert(k, c.clone());
                }
                for c in &inner.cookies {
                    let k = c.key();
                    if inner.dirty.contains(&k) && !c.expired(now_ms) {
                        if !map.contains_key(&k) {
                            order.push(k.clone());
                        }
                        map.insert(k, c.clone());
                    }
                }
                order
                    .into_iter()
                    .filter_map(|k| map.remove(&k))
                    .collect()
            };
            let text = cookie_json::encode(&merged);
            // 当前的三样都写进文件了，清空：之后别的进程再改同名 Cookie，不会被我们按旧账删掉
            inner.dirty.clear();
            inner.removed.clear();
            inner.cleared_domains.clear();
            inner.pending_persist = false;
            text
        };
        self.storage.write(&text).map_err(StorageError::Write)?;
        let new_stamp = self.storage.file_stamp();
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .last_disk_stamp = new_stamp;
        Ok(())
    }

    /// 磁盘上那一份；没有文件或者读不出来返回 `None`（调用方按"整份写回"处理）。
    fn read_stored(&self) -> Option<Vec<StoredCookie>> {
        let text = self.storage.read()?;
        cookie_json::decode(&text).ok()
    }
}

impl Drop for CookieJar {
    fn drop(&mut self) {
        if let Some(mut worker) = self
            .flusher
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
        {
            {
                let (lock, cv) = &*worker.shared;
                let mut st = lock.lock().unwrap_or_else(|e| e.into_inner());
                st.shutdown = true;
                st.due_at = None;
                cv.notify_all();
            }
            if let Some(join) = worker.join.take() {
                if join.thread().id() != std::thread::current().id() {
                    let _ = join.join();
                }
            }
        }
        let now_ms = self
            .inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .last_now_ms;
        let _ = self.flush_pending(now_ms);
    }
}

/// 落盘失败（`cookies.json` 写不进去）。
#[derive(Debug)]
pub enum StorageError {
    Write(String),
}

impl std::fmt::Display for StorageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Write(detail) => write!(f, "写 cookies.json 失败：{detail}"),
        }
    }
}

/// "什么也不存"的落盘（内存罐子）。
struct NoStorage;

impl CookieStorage for NoStorage {
    fn read(&self) -> Option<String> {
        None
    }

    fn write(&self, _text: &str) -> Result<(), String> {
        Ok(())
    }

    fn quarantine(&self) -> Option<String> {
        None
    }
}

// ===========================================================================
// WebView / 浏览器请求头 → 罐条目
// ===========================================================================

/// 把浏览器请求头里的 `Cookie: a=1; b=2` 解析成 Cookie，统一记到 `domain`（Kotlin `parseCookieHeader`）。
///
/// 浏览器请求头里没有过期时间，这里给**一年**（和 Kotlin 一样，真失效以服务端为准）。口径逐条照
/// `core/Cookies.kt`：
/// - 先去掉前缀 `Cookie:` / `cookie:`（**只去一次、大小写各一种**）；
/// - 按 `;` 切；每段取**第一个** `=`，下标 ≤ 0 的段丢掉（`=nokey` / 没有 `=`）；
/// - 名字 / 值各自 `trim()`（Kotlin 的 `String.trim()` = 去两端 Java 空白）；
/// - 名字为空丢掉；
/// - `domain` 去掉前导点（**域 Cookie**，不是 hostOnly）；`path = "/"`；`expiresAt = now + 365 天`。
///
/// 注意 Kotlin 那边 `Cookie.Builder().value(value)` 会**校验**值（换行之类会抛），这里照抄"解析"这一半，
/// 校验在调用方做（真实来源要么是浏览器的请求头、要么是我们自己的存档，两边都不带换行）。
pub fn parse_cookie_header(header: &str, domain: &str, now_ms: i64) -> Vec<StoredCookie> {
    let d = domain.trim_start_matches('.');
    let expires = now_ms + 365 * 24 * 3600 * 1000;
    let rest = header
        .strip_prefix("Cookie:")
        .or_else(|| header.strip_prefix("cookie:"))
        .unwrap_or(header);
    let mut out = Vec::new();
    for part in rest.split(';') {
        let Some(idx) = part.find('=') else {
            continue;
        };
        if idx == 0 {
            // Kotlin 是 `indexOf('=')` 后判 `idx <= 0`：`=x` 这种（名字空）丢掉
            continue;
        }
        let name = kotlin_trim(&part[..idx]);
        let value = kotlin_trim(&part[idx + 1..]);
        if name.is_empty() {
            continue;
        }
        out.push(StoredCookie {
            name: name.to_string(),
            value: value.to_string(),
            domain: d.to_string(),
            path: "/".to_string(),
            expires_at_ms: expires,
            secure: false,
            http_only: false,
            host_only: false,
        });
    }
    out
}

/// Kotlin `String.trim()`：去掉两端的 Java 空白（`Character.isWhitespace`，**不含** NBSP）。
///
/// 复用 [`crate::hosted::kotlin_trim`]（那一份是 Java `Character.isWhitespace` 的口径，正好是 Kotlin
/// `trim()` 用的那个 `Char.isWhitespace` 的一部分——Kotlin 的 `trim()` 走 `Char.isWhitespace`，
/// 它比 `Character.isWhitespace` **多** NBSP 类三个字符。`parseCookieHeader` 里的名字 / 值不会有 NBSP，
/// 这里用同一个口径就行；真要逐字符对齐见 [`kotlin_ws_trim`]）。
fn kotlin_trim(s: &str) -> &str {
    crate::hosted::kotlin_trim(s)
}

/// Kotlin `Char.isWhitespace` 的真口径（`Character.isWhitespace || Character.isSpaceChar`）。
///
/// 和 [`crate::hosted::is_kotlin_blank`] 同一份判断；单独开一个函数是为了让 `trim` 的调用点看起来
/// 就是 Kotlin 那句。
pub fn kotlin_ws_trim(s: &str) -> &str {
    let is_ws = |c: char| {
        crate::hosted::is_java_whitespace(c) || matches!(c, '\u{a0}' | '\u{2007}' | '\u{202f}')
    };
    let start = s.char_indices().find(|(_, c)| !is_ws(*c)).map(|(i, _)| i).unwrap_or(s.len());
    let end = s
        .char_indices()
        .rev()
        .find(|(_, c)| !is_ws(*c))
        .map(|(i, c)| i + c.len_utf8())
        .unwrap_or(start);
    if end <= start {
        ""
    } else {
        &s[start..end]
    }
}

/// 一条响应里的 `Set-Cookie` → 一条 Cookie（**`okhttp3.Cookie.parse` 的逐条移植**）。
///
/// 为什么需要它：Rust 的联网层收到响应后要把 `Set-Cookie` 交给罐子，
/// 而罐子的 `save_from_response` 收的是**已经解析好的** [`StoredCookie`]。以前这一步在 OkHttp 里
/// （`Cookie.parse(url, header)`），切到 Rust 之后必须有一份逐字段一致的解析，否则
/// domain / path / secure / hostOnly / 过期时间任一项算错都是"用户以为登录了其实没有"。
///
/// 口径逐条照 OkHttp 4.12 的 `Cookie.parse` / `parseExpires` / `domainMatch`，包括它的怪癖：
/// - **`Max-Age` 优先于 `Expires`**（先看到的 `max-age` 赢；`max-age` 不合法就当没写、继续看后面的）；
/// - `Max-Age` 是**相对秒数**，`<= 0` 就是过期（`expiresAt` 给 `i64::MIN` 那一档 = 立刻过期）；
/// - `Expires` 认两种写法（`EEE, dd MMM yyyy HH:mm:ss zzz` 和 `EEE, dd-MMM-yyyy HH:mm:ss zzz`，
///   以及 `format2` 那种省略时分秒的），解析不出来就**忽略这个属性**（不是整条丢掉）；
/// - `domain` 带前导点照收（`trim_start_matches('.')` 之后**不带点**存），**host-only 只在没写
///   `domain` 属性时成立**；写了 `domain` 但其实等于 host 时 OkHttp 也一样按域 Cookie 处理；
/// - `path` 写了就用（必须以 `/` 开头，否则忽略）；没写就按 URL 的路径算**默认路径**
///   （最后一个 `/` 之前那段，没有就是 `/`）；
/// - `secure` / `httponly` 无条件的开关。
///
/// `url` 是**这一跳的最终地址**（重定向之后），和 OkHttp 的 `Cookie.parse(url, header)` 一样。
/// 返回 `None` = 这条 `Set-Cookie` 不合法（OkHttp 抛 `IllegalArgumentException`，这边丢掉这一条，
/// **不影响同一响应里的其它条**）。
pub fn parse_set_cookie(url: &HttpUrl, header: &str, now_ms: i64) -> Option<StoredCookie> {
    // OkHttp：`setCookie.split(";")` 之后 index 0 是 `name=value`。
    let mut parts = header.split(';');
    let pair = parts.next()?;
    let pair = pair.trim_matches(|c: char| c.is_ascii_whitespace() || c == '\t');
    if pair.is_empty() {
        return None;
    }
    // OkHttp：`indexOf('=') == -1` 或 `== 0` → 抛（`=value` 这种没有名字）
    let (name, value) = pair.split_once('=')?;
    if name.is_empty() {
        return None;
    }
    let name = name.trim_end_matches(|c: char| c.is_ascii_whitespace());
    if name.is_empty() || !is_valid_cookie_name(name) {
        return None;
    }
    let value = value.trim_matches(|c: char| c.is_ascii_whitespace());

    let mut expires_at_ms: Option<i64> = None;
    let mut max_age_seen = false;
    let mut domain: Option<String> = None;
    let mut path: Option<String> = None;
    let mut secure = false;
    let mut http_only = false;

    for part in parts {
        let part = part.trim_matches(|c: char| c.is_ascii_whitespace());
        if part.is_empty() {
            continue;
        }
        // OkHttp：先找 `=`，没有就整段是 key、值是空串
        let (key, attr_value) = match part.split_once('=') {
            Some((k, v)) => (k.trim_end_matches(|c: char| c.is_ascii_whitespace()), v),
            None => (part, ""),
        };
        // **`Max-Age` 赢了就不再看别的过期属性**（OkHttp 的循环里 `expiresAt == MAX_DATE`
        // 这个条件只在"还没被 Max-Age 设过"时成立；这里用 `max_age_seen` 表达同一件事）。
        match key.to_ascii_lowercase().as_str() {
            "expires" => {
                if !max_age_seen && expires_at_ms.is_none() {
                    if let Some(ms) = parse_cookie_date(attr_value.trim()) {
                        expires_at_ms = Some(ms);
                    }
                }
            }
            "max-age" => {
                if !max_age_seen {
                    if let Ok(secs) = attr_value.trim().parse::<i64>() {
                        max_age_seen = true;
                        // OkHttp：`delta <= 0` → `MAX_DATE`（= 立刻过期）；否则 `now + delta*1000`
                        expires_at_ms = Some(if secs <= 0 {
                            i64::MIN
                        } else {
                            now_ms.saturating_add(secs.saturating_mul(1000))
                        });
                    }
                }
            }
            "domain" => {
                if domain.is_none() {
                    let d = attr_value.trim();
                    if !d.is_empty() {
                        // OkHttp：`domain.startsWith(".")` 就去掉那一个点（**只去一个**）
                        domain = Some(d.strip_prefix('.').unwrap_or(d).to_string());
                    }
                }
            }
            "path" => {
                if path.is_none() {
                    let p = attr_value.trim();
                    if p.starts_with('/') {
                        path = Some(p.to_string());
                    }
                }
            }
            "secure" => secure = true,
            "httponly" => http_only = true,
            _ => {}
        }
    }

    // 域名：没写 `domain` → host-only，domain 就是 URL 的 host
    let (domain, host_only) = match domain {
        Some(d) => (d.to_ascii_lowercase(), false),
        None => (url.host.to_ascii_lowercase(), true),
    };
    // OkHttp 会校验"这个 domain 匹配不匹配这个 URL"（`domainMatch`），不匹配就整条丢掉
    if !host_only && !domain_match(&url.host, &domain) {
        return None;
    }
    // 路径：没写就按 URL 算默认路径
    let path = path.unwrap_or_else(|| default_cookie_path(&url.encoded_path));

    Some(StoredCookie {
        name: name.to_string(),
        value: value.to_string(),
        domain,
        path,
        expires_at_ms: expires_at_ms.unwrap_or(i64::MAX),
        secure,
        http_only,
        host_only,
    })
}

/// 一条响应里的**全部** `Set-Cookie` → Cookie 列表（按响应里的顺序；不合法的**丢掉那一条**）。
///
/// 对应 `CookieJar.saveFromResponse(url, cookies)`：OkHttp 那边 `cookies` 已经是 `Cookie.parse`
/// 的结果（解析失败的那一条在 OkHttp 里是抛异常 → 整条响应处理失败），这边选择**跳过**——
/// 红线上"服务端下发的 Cookie 越多越好"（多收一条不影响什么，少收一条就可能少一个登录态）。
pub fn parse_set_cookies(url: &HttpUrl, headers: &[String], now_ms: i64) -> Vec<StoredCookie> {
    headers
        .iter()
        .filter_map(|h| parse_set_cookie(url, h, now_ms))
        .collect()
}

/// OkHttp `Cookie.parse` 对名字的要求：**非空、不含控制字符和分隔符**。
///
/// OkHttp 4.12 里这条校验在 `Cookie.Builder.name()`（`"name is not valid: $name"`），
/// 由 `Cookie.parse` 走到；这边照 `HttpHeaderValue` 的口径：不能有 `\t` / 控制字符 / 这几个分隔符。
fn is_valid_cookie_name(name: &str) -> bool {
    name.chars().all(|c| {
        !c.is_control()
            && !matches!(
                c,
                '(' | ')' | '<' | '>' | '@' | ',' | ';' | ':' | '\\' | '"' | '/' | '[' | ']' | '?'
                    | '=' | '{' | '}' | ' '
            )
    })
}

/// OkHttp `Cookie.parse` 的默认路径：URL 路径里**最后一个 `/` 之前**那一段；没有 `/` 就是 `/`。
///
/// 例子：`/` → `/`；`/foo` → `/`；`/foo/bar` → `/foo`；`/foo/` → `/foo`。
fn default_cookie_path(encoded_path: &str) -> String {
    match encoded_path.rfind('/') {
        Some(0) | None => "/".to_string(),
        Some(i) => encoded_path[..i].to_string(),
    }
}

/// `Expires=Wed, 21 Oct 2015 07:28:00 GMT` → Unix 毫秒（OkHttp `Cookie.parseExpires` 的移植）。
///
/// 认 OkHttp 认的那三种：`EEE, dd MMM yyyy HH:mm:ss zzz`、`EEE, dd-MMM-yyyy HH:mm:ss zzz`、
/// `EEE, dd MMM yyyy HH:mm:ss zzz` 的省略时分秒版。认不出给 `None`（**忽略这个属性**）。
fn parse_cookie_date(text: &str) -> Option<i64> {
    // OkHttp 的 `DATE_PATTERNS` 都是"去掉逗号之前的星期几"再切
    let rest = match text.split_once(',') {
        Some((_, rest)) => rest,
        None => text,
    }
    .trim();
    if rest.is_empty() {
        return None;
    }
    let tokens: Vec<&str> = rest.split_whitespace().collect();
    if tokens.is_empty() {
        return None;
    }
    // 两种写法：`dd MMM yyyy HH:mm:ss …`（三/四个 token）或 `dd-MMM-yyyy HH:mm:ss …`（两个 token）
    let (day, month, year, time_part) = if tokens[0].contains('-') {
        let mut it = tokens[0].split('-');
        let day: i64 = it.next()?.trim().parse().ok()?;
        let month = month_number(it.next()?)?;
        let year: i64 = it.next()?.trim().parse().ok()?;
        (day, month, year, tokens.get(1).copied())
    } else {
        if tokens.len() < 3 {
            return None;
        }
        let day: i64 = tokens[0].parse().ok()?;
        let month = month_number(tokens[1])?;
        let year: i64 = tokens[2].parse().ok()?;
        (day, month, year, tokens.get(3).copied())
    };
    let (hour, minute, second) = match time_part {
        None => (0, 0, 0),
        Some(t) => {
            let fields: Vec<&str> = t.split(':').collect();
            if fields.len() < 2 {
                return None;
            }
            let hour: i64 = fields[0].parse().ok()?;
            let minute: i64 = fields[1].parse().ok()?;
            let second: i64 = if fields.len() > 2 { fields[2].parse().ok()? } else { 0 };
            (hour, minute, second)
        }
    };
    if !(1..=31).contains(&day) || hour > 23 || minute > 59 || second > 60 {
        return None;
    }
    let days = days_from_civil(year, month, day);
    Some((days * 86_400 + hour * 3_600 + minute * 60 + second) * 1000)
}

/// `Jan` / `January` → `1`（认不出给 `None`）。
fn month_number(text: &str) -> Option<i64> {
    let lower = text.trim().to_ascii_lowercase();
    let key = &lower[..lower.len().min(3)];
    Some(match key {
        "jan" => 1,
        "feb" => 2,
        "mar" => 3,
        "apr" => 4,
        "may" => 5,
        "jun" => 6,
        "jul" => 7,
        "aug" => 8,
        "sep" => 9,
        "oct" => 10,
        "nov" => 11,
        "dec" => 12,
        _ => return None,
    })
}

/// 公历 → 纪元日（`hosted::civil_from_days` 的逆；和 `tieba::qr` 那份同口径）。
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// 一串 `Cookie` 请求头（WebView 的 `CookieManager.getCookie(url)` 可能给好几段）→ Cookie 列表，
/// **同名只留第一个**（Kotlin `WebLoginCookies.parse`）。
pub fn parse_cookie_headers(headers: &[String], domain: &str, now_ms: i64) -> Vec<StoredCookie> {
    let mut out: Vec<StoredCookie> = Vec::new();
    for h in headers {
        if crate::hosted::is_kotlin_blank(h) {
            continue;
        }
        for c in parse_cookie_header(h, domain, now_ms) {
            if out.iter().any(|x| x.name == c.name) {
                continue;
            }
            out.push(c);
        }
    }
    out
}

/// `cookies.json` 的**真实落盘 + 跨进程文件锁**。
///
/// 老 cli 进程和新
/// Rust 实现能同时开着互不丢改动）。宿主只剩一句"文件在哪"。见 [`file`] 的模块头。
pub mod file;

pub use file::FileStorage;

