//! 翻译磁盘缓存：`~/.cache/feedhub/translations.jsonl`（一行一个 JSON：`{"k":…,"v":…}`）。
//!
//! 口径（`app/.../translate/Translation.kt` 的 `TranslationCache`）**一个字节都不改**：
//! - 键是 SHA-256（64 位小写十六进制）算出来的，原文是
//!   `"<版本>\0<模型路径>\0<目标语言>\0<正文>"`（见 [`key_for`]）；
//! - 一行一个 `{"k":"<键>","v":"<译文>"}`，**追加写**，UTF-8，行尾 `\n`；
//! - 读的时候跳过空行、坏行忽略（`runCatching`）；
//! - 同一个键写第二次不追加（内存里已经有了）；
//! - `clear()` 清内存 + 删文件。
//!
//! 和 Kotlin 的差别只有一个：Kotlin 用一个单线程执行器排队写（译文在 Main 上回来，不能占 UI 线程）；
//! 这边 [`Cache::put`] 是**同步写**，宿主照样在它自己的写线程上调（见 `RustTranslationCache`），
//! 所以"不占 Main"这条不变，只是排队从 Kotlin 挪到了宿主的那个写线程。
//!
//! 对拍：Kotlin 版写出来的 `translations.jsonl` 给这里读、这里写出来的给 Kotlin 版读，
//! 两边逐字节相同（`TranslateCacheParityTest`）。

use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

/// 缓存格式版本；判定方式改了要加一（旧译文不会再被取出来）。
///
/// **`lid2` 起定在 Rust 这边**（和 Kotlin 原版同一个字符串，老缓存照样命中）。
pub const CACHE_VERSION: &str = "lid2";

/// 缓存文件名（`cacheDir/translations.jsonl`）。
pub const CACHE_FILE: &str = "translations.jsonl";

/// 缓存键：`SHA-256("<版本>\0<模型路径>\0<目标语言>\0<正文>")` 的小写十六进制。
///
/// 模型路径也进键（`lid2` 起取代原来的引擎 id）：引擎是懒建的，键不能等它建出来才稳定，
/// 换个 GGUF 就换一批键，旧译文不会串（第一次会把旧缓存当作未命中、重新翻一遍，纯性能代价）。
pub fn key_for(version: &str, model_path: &str, target: &str, text: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(version.as_bytes());
    hasher.update([0u8]);
    hasher.update(model_path.as_bytes());
    hasher.update([0u8]);
    hasher.update(target.as_bytes());
    hasher.update([0u8]);
    hasher.update(text.as_bytes());
    let digest = hasher.finalize();
    let mut out = String::with_capacity(digest.len() * 2);
    for b in digest {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

/// 一行缓存的编码（`kotlinx.serialization` 的 `Entry(k, v)` 在没有空格的情况下就是这样）。
pub fn encode_line(key: &str, text: &str) -> String {
    let mut out = String::with_capacity(key.len() + text.len() + 16);
    out.push_str("{\"k\":");
    crate::hosted::push_json_string(&mut out, key);
    out.push_str(",\"v\":");
    crate::hosted::push_json_string(&mut out, text);
    out.push('}');
    out
}

/// 一行缓存的解码：返回 `(key, value)`；坏行 / 缺字段返回 `None`（和 Kotlin 的 `runCatching` 一样跳过）。
pub fn decode_line(line: &str) -> Option<(String, String)> {
    let value: serde_json::Value = serde_json::from_str(line).ok()?;
    let obj = value.as_object()?;
    let k = obj.get("k")?.as_str()?.to_string();
    let v = obj.get("v")?.as_str()?.to_string();
    Some((k, v))
}

/// 磁盘缓存的全部状态（内存表 + 文件句柄位置）。
pub struct Cache {
    file: Option<PathBuf>,
    entries: HashMap<String, String>,
}

impl Cache {
    /// 从 [`dir`] 里读 `translations.jsonl`；`None` = 只放内存（安卓 / 测试 / 截图）。
    ///
    /// 读失败（文件坏了 / 没权限）就当作空缓存：缓存只是省重复请求，不是数据。
    pub fn load(dir: Option<&Path>) -> Cache {
        let file = dir.map(|d| d.join(CACHE_FILE));
        let mut entries = HashMap::new();
        if let Some(path) = &file {
            if let Ok(content) = std::fs::read_to_string(path) {
                for line in content.lines() {
                    if line.trim().is_empty() {
                        continue;
                    }
                    if let Some((k, v)) = decode_line(line) {
                        entries.insert(k, v);
                    }
                }
            }
        }
        Cache { file, entries }
    }

    /// 只放内存的缓存（测试用）。
    pub fn in_memory() -> Cache {
        Cache {
            file: None,
            entries: HashMap::new(),
        }
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.entries.get(key).map(|s| s.as_str())
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// 写一条：内存里**已经有了就不追加**（和 Kotlin 的 `if (entries.put(...) != null) return` 一样）。
    ///
    /// 返回 `true` = 真的追加了一行；`false` = 命中过（没写盘）。
    /// 写盘失败不报错（和 Kotlin 的 `runCatching` 一样：缓存写不进去只是下次再翻一遍）。
    pub fn put(&mut self, key: &str, text: &str) -> bool {
        if self.entries.contains_key(key) {
            self.entries.insert(key.to_string(), text.to_string());
            return false;
        }
        self.entries.insert(key.to_string(), text.to_string());
        let Some(path) = &self.file else {
            return true;
        };
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let line = format!("{}\n", encode_line(key, text));
        match OpenOptions::new().create(true).append(true).open(path) {
            Ok(mut f) => {
                let _ = f.write_all(line.as_bytes());
                let _ = f.flush();
                true
            }
            Err(_) => true,
        }
    }

    /// 清掉内存和文件（和 Kotlin 的 `clear()` 一样：文件删不掉也不报错）。
    pub fn clear(&mut self) {
        self.entries.clear();
        if let Some(path) = &self.file {
            let _ = std::fs::remove_file(path);
        }
    }

    /// 缓存文件路径（测试 / 排障看一眼写到哪儿了）。
    pub fn path(&self) -> Option<&Path> {
        self.file.as_deref()
    }
}

