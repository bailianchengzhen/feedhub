//! **信息流上一批本地快照**。
//!
//! 对应 Kotlin 原版 `app/src/commonMain/kotlin/feedhub/app/feed/FeedSnapshot.kt`。
//!
//! 核心语义与口径：
//! - `MAX_ITEMS = 120`；
//! - `MAX_AGE_MS = 7 * 24 * 3600 * 1000`（7 天）；
//! - 文件名格式 `feed-${kind}.json`；
//! - 磁盘 JSON 格式逐字节与 Kotlin `FeedJson.encodeToString(Stored.serializer(), Stored(savedAt, items.take(120)))` 对齐：
//!   `{"savedAt":<i64>,"items":[<item1>,<item2>,...]}`；
//! - 写入先写 `.tmp` 临时文件，再通过 `fs::rename` 原子替换，任何失败静默不抛；
//! - 读取时校验普通文件、解析顶层对象、校验 `now - savedAt > MAX_AGE_MS` 过期判定。

use std::fs;
use std::path::{Path, PathBuf};

pub const MAX_ITEMS: usize = 120;
pub const MAX_AGE_MS: i64 = 7 * 24 * 3600 * 1000;

/// 首页 / 关注各一个文件名。
pub fn file_name(kind: &str) -> String {
    format!("feed-{}.json", kind.to_ascii_lowercase())
}

/// 存这一批（最上面 `MAX_ITEMS` 条）。
///
/// 失败静默返回 false，不 panic。
pub fn save_snapshot(path: &str, saved_at: i64, items_json: &[String]) -> bool {
    let p = Path::new(path);
    if let Some(parent) = p.parent() {
        if !parent.as_os_str().is_empty() && fs::create_dir_all(parent).is_err() {
            return false;
        }
    }

    let file_name = match p.file_name().and_then(|n| n.to_str()) {
        Some(name) => name,
        None => return false,
    };
    let tmp_path: PathBuf = p.with_file_name(format!("{file_name}.tmp"));

    let items_to_write = &items_json[..items_json.len().min(MAX_ITEMS)];
    let mut payload = format!(r#"{{"savedAt":{saved_at},"items":["#);
    for (i, item) in items_to_write.iter().enumerate() {
        if i > 0 {
            payload.push(',');
        }
        payload.push_str(item);
    }
    payload.push_str("]}");

    if fs::write(&tmp_path, payload.as_bytes()).is_err() {
        let _ = fs::remove_file(&tmp_path);
        return false;
    }

    if fs::rename(&tmp_path, p).is_err() {
        let _ = fs::remove_file(&tmp_path);
        return false;
    }

    true
}

/// 读上一批；没有、坏了、非普通文件、太旧都给空列表。
pub fn load_snapshot(path: &str, now: i64) -> Vec<String> {
    let p = Path::new(path);
    let meta = match fs::metadata(p) {
        Ok(m) => m,
        Err(_) => return Vec::new(),
    };
    if !meta.is_file() {
        return Vec::new();
    }

    let bytes = match fs::read(p) {
        Ok(b) => b,
        Err(_) => return Vec::new(),
    };

    let text = match std::str::from_utf8(&bytes) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };

    let root = match crate::sync::json::parse(text) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };

    let saved_at = match root.i64_field("savedAt") {
        Some(s) => s,
        None => return Vec::new(),
    };

    // 过期判定：原 Kotlin 口径 `now() - stored.savedAt > MAX_AGE_MS`
    if now.saturating_sub(saved_at) > MAX_AGE_MS {
        return Vec::new();
    }

    let items_array = match root.get("items").and_then(|v| v.as_array()) {
        Some(arr) => arr,
        None => return Vec::new(),
    };

    items_array.iter().map(crate::sync::json::to_json_string).collect()
}

