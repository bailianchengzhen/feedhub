//! 用户正等待的平台请求计量规则与文案。

/// 交互式请求的总超时（30 秒）。
pub const INTERACTIVE_REQUEST_TIMEOUT_MS: i64 = 30_000;

/// 超时转成网络错误时的异常文案。
pub fn timeout_message(timeout_ms: i64) -> String {
    format!("请求超过 {timeout_ms} ms 没返回，已放弃（可以重试）")
}

/// 失败请求的日志行拼装（logcat / stderr 统一格式）。
pub fn request_failed_log_line(
    kind: &str,
    platform: &str,
    reason: &str,
    ms: i64,
    queue_ms: Option<i64>,
) -> String {
    let queue_part = queue_ms
        .map(|q| format!("（限速排队 {q}ms）"))
        .unwrap_or_default();
    format!("[{kind}] {platform} 失败：{reason}，{ms}ms{queue_part}")
}

