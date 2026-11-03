//! 扫码登录的共用件过 UniFFI 的那一层。
//!
//! 两样东西：
//! - [`FfiQrSessions`]：扫码会话的 **LRU**（Kotlin `QrSessions`）。**Cookie 罐本身在宿主**
//!   （`MemoryCookieJar` 是 OkHttp 的对象），这里只有"谁排在哪、谁该被挤掉"；
//!   返回的被挤掉的 token 由宿主拿去删自己那份罐子。
//! - [`qr_parse_object`]：二维码接口响应的解析 + 判错（Kotlin `parseQrJson`）。是 JSON 对象就返回
//!   **紧凑 JSON**，否则 `None`——调用方抛 `ParseException`，**文案里不带响应内容**（成功响应里有凭据）。

use std::sync::Mutex;

use feedhub_core::qr::{self, Sessions};

/// `QrSessions(maxSize)` 的默认容量（Kotlin `QrSessions()`）。
#[uniffi::export]
pub fn qr_default_max_size() -> u32 {
    qr::sessions::DEFAULT_MAX_SIZE as u32
}

/// 解析失败的两档（对应 Kotlin `parseQrJson` 的两个 `catch` 分支，文案不同）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FfiQrJsonError {
    /// 解析成功。
    None,
    /// 不是合法 JSON。
    Invalid,
    /// 是 JSON，但不是对象（数组 / 标量）。
    NotObject,
}

/// 二维码接口响应的解析结果。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiQrJson {
    /// 成功时是**紧凑 JSON**（宿主交给 kotlinx 变成 `JsonObject`）。
    pub json: Option<String>,
    pub error: FfiQrJsonError,
}

/// 二维码接口的响应 → 紧凑 JSON（判错在这一层；文案由宿主按 `error` 拼，**不带响应内容**）。
#[uniffi::export]
pub fn qr_parse_object(body: String) -> FfiQrJson {
    match qr::parse(&body) {
        Ok(json) => FfiQrJson {
            json: Some(json),
            error: FfiQrJsonError::None,
        },
        Err(qr::ParseError::Invalid) => FfiQrJson {
            json: None,
            error: FfiQrJsonError::Invalid,
        },
        Err(qr::ParseError::NotObject) => FfiQrJson {
            json: None,
            error: FfiQrJsonError::NotObject,
        },
    }
}

/// `QrSessions.jar(token)` 的结果。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiQrJar {
    /// 登记表里原来有没有这个 token（`false` = 宿主新建一个空罐）。
    pub had: bool,
    /// 被挤掉的 token（宿主删掉它那份罐子）。
    pub evicted: Option<String>,
}

/// 扫码会话的 LRU（键 = 平台给的 token / 轮询 key）。
#[derive(uniffi::Object)]
pub struct FfiQrSessions {
    inner: Mutex<Sessions>,
}

#[uniffi::export]
impl FfiQrSessions {
    /// `QrSessions(maxSize)`。0 当成 1（和 Kotlin 的 `removeEldestEntry { size > maxSize }` 等效）。
    #[uniffi::constructor]
    pub fn new(max_size: u32) -> Self {
        FfiQrSessions {
            inner: Mutex::new(Sessions::new(max_size as usize)),
        }
    }

    /// `begin(token, jar)`：登记 / 覆盖，返回被挤掉的 token。
    pub fn begin(&self, token: String) -> Option<String> {
        self.lock().begin(&token)
    }

    /// `jar(token)`：已有的算一次访问、不挤人；没有的排到最新（这时可能挤掉别人）。
    pub fn jar(&self, token: String) -> FfiQrJar {
        let (had, evicted) = self.lock().jar(&token);
        FfiQrJar { had, evicted }
    }

    /// `end(token)`：终态后丢掉。
    pub fn end(&self, token: String) -> bool {
        self.lock().end(&token)
    }

    /// 现在记着几条（排障 / 测试断言用）。
    pub fn len(&self) -> u32 {
        self.lock().len() as u32
    }

    /// 当前顺序，最久没访问的在前（排障 / 测试断言用）。
    pub fn tokens(&self) -> Vec<String> {
        self.lock().tokens()
    }
}

impl FfiQrSessions {
    /// **不是**导出方法（`#[uniffi::export]` 的 impl 块里每个 `fn` 都会被导出，锁的守卫过不了 FFI）。
    fn lock(&self) -> std::sync::MutexGuard<'_, Sessions> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }
}
