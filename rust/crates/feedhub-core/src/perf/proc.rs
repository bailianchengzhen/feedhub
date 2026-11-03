//! `/proc` 里 CPU 时间的解析与线程增量计算。

use std::collections::HashMap;

/// Linux 用户态 clock tick 的频率：固定 100（1 tick = 10ms）。
pub const PERF_CLOCK_TICKS_PER_SEC: i64 = 100;

/// `/proc/self/stat` 或 `/proc/self/task/<tid>/stat` 解析出来的 ticks。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcTicks {
    pub pid: i32,
    pub name: String,
    pub utime_ticks: i64,
    pub stime_ticks: i64,
}

impl ProcTicks {
    pub fn new(pid: i32, name: String, utime_ticks: i64, stime_ticks: i64) -> Self {
        Self {
            pid,
            name,
            utime_ticks,
            stime_ticks,
        }
    }

    pub fn total_ticks(&self) -> i64 {
        self.utime_ticks.saturating_add(self.stime_ticks)
    }
}

/// 解析一行 `/proc/.../stat`；格式不对返回 None。
///
/// 第 2 个字段 comm 是带括号的名字，可能有空格甚至括号（如 `(Web Content)`、`((weird) name)`），
/// 所以按第一个 `(` 到最后一个 `)` 截取名字，后面的字段按空格切分。
pub fn parse_proc_stat(text: &str) -> Option<ProcTicks> {
    let line = text
        .lines()
        .find(|l| !crate::html::kotlin_is_blank(l))?;

    let open = line.find('(')?;
    let close = line.rfind(')')?;
    if close <= open {
        return None;
    }

    let pid_str = crate::html::kotlin_trim(&line[..open]);
    let pid = pid_str.parse::<i32>().ok()?;
    let name = line[open + 1..close].to_string();

    let rest_str = crate::html::kotlin_trim(&line[close + 1..]);
    let rest: Vec<&str> = rest_str.split(' ').filter(|s| !s.is_empty()).collect();
    // ')' 之后：state(3) ppid(4) ... utime(14) stime(15)；相对索引 0 是字段 3
    if rest.len() < 13 {
        return None;
    }
    let utime = rest[11].parse::<i64>().ok()?;
    let stime = rest[12].parse::<i64>().ok()?;

    Some(ProcTicks {
        pid,
        name,
        utime_ticks: utime,
        stime_ticks: stime,
    })
}

/// 一个线程在两次采样之间用掉的 CPU（毫秒）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadCpu {
    pub name: String,
    pub ms: i64,
}

/// 按线程算两次采样之间的增量，取最大的 `limit` 个。
///
/// 稳定排序：毫秒相同时保持 `current` 原有先后顺序（与 Kotlin `sortedByDescending` 行为一致）。
pub fn top_thread_deltas(
    previous: &HashMap<i32, ProcTicks>,
    current: &[ProcTicks],
    limit: usize,
    ticks_per_sec: i64,
) -> Vec<ThreadCpu> {
    let mut deltas: Vec<ThreadCpu> = current
        .iter()
        .filter_map(|now| {
            let before = previous.get(&now.pid)?;
            let delta = now.total_ticks() - before.total_ticks();
            if delta <= 0 {
                None
            } else {
                Some(ThreadCpu {
                    name: now.name.clone(),
                    ms: ticks_to_ms(delta, ticks_per_sec),
                })
            }
        })
        .collect();

    // slice::sort_by 是稳定的归并排序（Timsort / Drift sort），同 ms 时保持 current 顺序
    deltas.sort_by(|a, b| b.ms.cmp(&a.ms));
    deltas.truncate(limit);
    deltas
}

/// tick 差 → 毫秒（向下取整；1 tick 在 100Hz 下就是 10ms）。
pub fn ticks_to_ms(ticks: i64, ticks_per_sec: i64) -> i64 {
    if ticks_per_sec <= 0 {
        0
    } else {
        ((ticks as i128 * 1000) / (ticks_per_sec as i128)) as i64
    }
}

/// 非负增量：变小或任一为 None 时返回 None。
pub fn delta_or_null(before: Option<i64>, now: Option<i64>) -> Option<i64> {
    match (before, now) {
        (Some(b), Some(n)) => n.checked_sub(b).filter(|&d| d >= 0),
        _ => None,
    }
}

/// 有符号增量：允许负值（给电池电荷计数器等使用）。
pub fn signed_delta_or_null(before: Option<i64>, now: Option<i64>) -> Option<i64> {
    match (before, now) {
        (Some(b), Some(n)) => n.checked_sub(b),
        _ => None,
    }
}

/// 直接读本进程 `/proc/self/stat`。
pub fn read_proc_self_stat() -> Option<ProcTicks> {
    std::fs::read_to_string("/proc/self/stat")
        .ok()
        .and_then(|text| parse_proc_stat(&text))
}

/// 直接遍历 `/proc/self/task/` 目录读取所有线程的 `/proc/self/task/<tid>/stat`。
/// 一次性在 Rust 里读取解析，避免在 JVM 层创建成百上千个 File/String 对象。
pub fn read_proc_self_threads() -> Vec<ProcTicks> {
    let Ok(entries) = std::fs::read_dir("/proc/self/task") else {
        return Vec::new();
    };
    let mut threads = Vec::new();
    for entry in entries.flatten() {
        let mut path = entry.path();
        path.push("stat");
        if let Ok(text) = std::fs::read_to_string(&path) {
            if let Some(ticks) = parse_proc_stat(&text) {
                threads.push(ticks);
            }
        }
    }
    threads
}

