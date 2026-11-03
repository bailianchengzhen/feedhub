//! 扫码登录的**共用件**。
//!
//! 各平台自己的扫码流程（临时 Cookie 罐、二维码生成 / 轮询、落地）在 `*/qr.rs` 里；这里只放**和平台无关**
//! 的两件事：
//!
//! | 模块 | Kotlin 原实现 | 为什么能搬 |
//! |---|---|---|
//! | [`sessions`] | `QrSessions`：8 条扫码会话的 LRU（`LinkedHashMap(accessOrder = true)`） | 纯状态机。**Cookie 罐本身还在宿主**（`MemoryCookieJar` 是 OkHttp 的对象），Rust 只记"谁排在哪、谁该被挤掉"，宿主按返回的键删罐子 |
//! | [`parse`] | `parseQrJson`：二维码接口响应的解析（必须是 JSON **对象**，出错信息**不带响应内容**） | 解析 + 判错在 Rust；宿主拿到的是一段紧凑 JSON，再交给 kotlinx 变成 `JsonObject`（那是宿主的模型表示） |
//!
//! 两条都不要时间、不要随机数、不发请求。

pub mod parse;
pub mod sessions;

pub use parse::{parse, parse_object, ParseError};
pub use sessions::Sessions;
