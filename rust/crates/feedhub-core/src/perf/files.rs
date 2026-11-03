//! 性能记录文件的存储与按天轮转、淘汰、清空。

use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// 记录文件保留几天（默认 7 天）。
pub const PERF_KEEP_DAYS: usize = 7;

/// 一天一个文件的名字前缀（`perf-YYYYMMDD.jsonl`）。
pub const PERF_FILE_PREFIX: &str = "perf-";

/// 安卓记录目录的名字（`getExternalFilesDir` 下面这一层）。
pub const ANDROID_DIR_NAME: &str = "perf";

/// 记录目录状态（文件数、总字节）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PerfStatus {
    pub dir_path: Option<String>,
    pub files: i32,
    pub bytes: i64,
}

impl PerfStatus {
    pub const UNSUPPORTED: Self = Self {
        dir_path: None,
        files: 0,
        bytes: 0,
    };

    pub fn supported(&self) -> bool {
        self.dir_path.is_some()
    }
}

/// 将毫秒时间戳 + 时区偏移（秒）格式化为 `YYYYMMDD`。
pub fn day_stamp(now_ms: i64, offset_sec: i32) -> String {
    let local_ms = now_ms.saturating_add((offset_sec as i64) * 1000);
    let day_number = local_ms.div_euclid(86_400_000);
    let (year, month, day) = crate::present::text::civil_from_days(day_number);
    format!("{year:04}{month:02}{day:02}")
}

/// 判断文件名是否是合法的 `perf-YYYYMMDD.jsonl`，如果是则返回 8 位日期字符串。
pub fn day_of_filename(name: &str) -> Option<&str> {
    let stripped = name.strip_prefix(PERF_FILE_PREFIX)?;
    let day = stripped.strip_suffix(".jsonl")?;
    if day.len() == 8 && day.chars().all(|c| c.is_ascii_digit()) {
        Some(day)
    } else {
        None
    }
}

struct StoreInner {
    writer: Option<BufWriter<File>>,
    writer_day: Option<String>,
    writer_dir: Option<String>,
    last_error: Option<String>,
    keep_days: usize,
}

impl StoreInner {
    fn close_writer_locked(&mut self) {
        if let Some(mut w) = self.writer.take() {
            let _ = w.flush();
        }
        self.writer_day = None;
        self.writer_dir = None;
    }

    fn list_matching_files(dir_path: &str) -> Vec<PathBuf> {
        let p = Path::new(dir_path);
        let Ok(entries) = std::fs::read_dir(p) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() {
                if let Some(file_name) = path.file_name().and_then(|n| n.to_str()) {
                    if day_of_filename(file_name).is_some() {
                        out.push(path);
                    }
                }
            }
        }
        out.sort_by(|a, b| a.file_name().cmp(&b.file_name()));
        out
    }

    fn prune_locked(&mut self, dir_path: &str, now_ms: i64, keep_from_offset_sec: i32) -> u32 {
        let keep_from_ms = now_ms.saturating_sub(
            (self.keep_days.saturating_sub(1) as i64).saturating_mul(86_400_000),
        );
        let keep_from = day_stamp(keep_from_ms, keep_from_offset_sec);
        let files = Self::list_matching_files(dir_path);
        let mut removed = 0u32;
        for f in files {
            if let Some(file_name) = f.file_name().and_then(|n| n.to_str()) {
                if let Some(day) = day_of_filename(file_name) {
                    if day < keep_from.as_str() {
                        if std::fs::remove_file(&f).is_ok() {
                            removed += 1;
                        }
                    }
                }
            }
        }
        removed
    }
}

/// 性能记录文件的存储核心（纯 Rust 实现）。
pub struct PerfFileStore {
    inner: Mutex<StoreInner>,
}

impl PerfFileStore {
    pub fn new(keep_days: usize) -> Self {
        Self {
            inner: Mutex::new(StoreInner {
                writer: None,
                writer_day: None,
                writer_dir: None,
                last_error: None,
                keep_days: if keep_days == 0 { PERF_KEEP_DAYS } else { keep_days },
            }),
        }
    }

    /// 追加一行（自动补换行 + flush）。返回写成功没有。
    pub fn append(
        &self,
        dir_path: Option<&str>,
        line: &str,
        now_ms: i64,
        offset_sec: i32,
        keep_from_offset_sec: i32,
    ) -> bool {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let Some(d) = dir_path else {
            inner.last_error = Some("没有可写的目录".to_string());
            return false;
        };

        let day = day_stamp(now_ms, offset_sec);
        let same_day_and_dir = inner.writer.is_some()
            && inner.writer_day.as_deref() == Some(&day)
            && inner.writer_dir.as_deref() == Some(d);

        if !same_day_and_dir {
            let previous_day = inner.writer_day.take();
            inner.close_writer_locked();

            // 换天时顺便清一次旧文件
            if previous_day.as_deref().is_some_and(|prev| prev != day) {
                inner.prune_locked(d, now_ms, keep_from_offset_sec);
            }

            if let Err(e) = std::fs::create_dir_all(d) {
                inner.last_error = Some(e.to_string());
                return false;
            }

            let file_path = PathBuf::from(d).join(format!("{PERF_FILE_PREFIX}{day}.jsonl"));
            let file = match OpenOptions::new().create(true).append(true).open(&file_path) {
                Ok(f) => f,
                Err(e) => {
                    inner.last_error = Some(e.to_string());
                    return false;
                }
            };
            inner.writer = Some(BufWriter::new(file));
            inner.writer_day = Some(day);
            inner.writer_dir = Some(d.to_string());
        }

        let writer = inner.writer.as_mut().unwrap();
        if let Err(e) = writer.write_all(line.as_bytes()).and_then(|_| writer.write_all(b"\n")).and_then(|_| writer.flush()) {
            inner.close_writer_locked();
            inner.last_error = Some(e.to_string());
            false
        } else {
            inner.last_error = None;
            true
        }
    }

    /// 删掉 `keep_days` 天以前的文件，返回删了几个。
    pub fn prune(
        &self,
        dir_path: Option<&str>,
        now_ms: i64,
        keep_from_offset_sec: i32,
    ) -> u32 {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let Some(d) = dir_path else {
            return 0;
        };
        inner.prune_locked(d, now_ms, keep_from_offset_sec)
    }

    /// 目录下所有合法的记录文件名（按文件名升序）。
    pub fn files(&self, dir_path: Option<&str>) -> Vec<String> {
        let _guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let Some(d) = dir_path else {
            return Vec::new();
        };
        StoreInner::list_matching_files(d)
            .into_iter()
            .filter_map(|p| p.file_name().and_then(|n| n.to_str()).map(|s| s.to_string()))
            .collect()
    }

    /// 目录状态统计。
    pub fn status(&self, dir_path: Option<&str>) -> PerfStatus {
        let _guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let Some(d) = dir_path else {
            return PerfStatus::UNSUPPORTED;
        };
        let files = StoreInner::list_matching_files(d);
        let total_bytes: u64 = files.iter().filter_map(|p| std::fs::metadata(p).ok().map(|m| m.len())).sum();
        PerfStatus {
            dir_path: Some(d.to_string()),
            files: files.len() as i32,
            bytes: total_bytes as i64,
        }
    }

    /// 清空所有记录文件。
    pub fn clear(&self, dir_path: Option<&str>) -> u32 {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.close_writer_locked();
        let Some(d) = dir_path else {
            return 0;
        };
        let files = StoreInner::list_matching_files(d);
        let mut removed = 0u32;
        for f in files {
            if std::fs::remove_file(&f).is_ok() {
                removed += 1;
            }
        }
        removed
    }

    /// 关掉正在写入的文件。
    pub fn shutdown(&self) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.close_writer_locked();
    }

    /// 上一次写盘失败的原因。
    pub fn last_error(&self) -> Option<String> {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.last_error.clone()
    }
}

