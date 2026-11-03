//! 跨平台搜索的混排（`core/search/SearchMerge.kt` 的移植）。
//!
//! 方案 B：**统一列表 + 平台配额轮转**。语义一条都没改：
//!
//! - 每家第一页只取 k 条进自己的桶，然后**按平台顺序轮转各取一条**拼成一批；
//! - `k = max(2, ceil(目标条数 / 平台数))`，但不超过这家自己的一页条数（`pageSize`）；
//! - 平台之间不做"相关度比较"（各家的分数不可比），这里只决定摆放顺序；
//! - 同一批里重复的 `FeedItem.key` 只留**第一条**；不在平台顺序里的平台不收。

use std::collections::HashSet;

use super::MixItem;

/// 一批的目标条数。
pub const TARGET_BATCH: i32 = 20;

/// 每家在**一页里**取几条进桶。
///
/// `platforms` 是当前参与的平台数，`page_size` 是这家一页给多少条（请求口径；响应可能更少，
/// 桶里以实际条数为准），`target` 是一批的目标条数。
pub fn quota(platforms: i32, page_size: i32, target: i32) -> i32 {
    if platforms <= 0 {
        return 0;
    }
    // Kotlin 的 Int 加法会回绕，这里照抄（参数都是小数字，只有边界用例才碰得到）
    let k = std::cmp::max(2, target.wrapping_add(platforms).wrapping_sub(1) / platforms);
    std::cmp::min(k, std::cmp::max(page_size, 1))
}

/// 轮转：按 `platforms` 里的平台顺序，一轮每家取一条，直到所有桶都空。
///
/// `items` 是各家结果拼在一起的整批（顺序无所谓，同一个平台的相对顺序会保留），返回**输入下标**。
/// 不在 `platforms` 里的平台不收（保持 `plugins.toml` 的平台顺序）；同一批里重复的 `key` 只留第一条。
pub fn round_robin(platforms: &[String], items: &[MixItem]) -> Vec<u32> {
    // 每个平台一个桶（桶内保持输入顺序）。空桶不进队列——空桶会让"当前有没有取到"永远为真。
    let mut queues: Vec<Vec<u32>> = Vec::new();
    for platform in platforms {
        let queue: Vec<u32> = items
            .iter()
            .enumerate()
            .filter(|(_, item)| item.platform == *platform)
            .map(|(i, _)| i as u32)
            .collect();
        if !queue.is_empty() {
            queues.push(queue);
        }
    }
    if queues.is_empty() {
        return Vec::new();
    }

    let mut out: Vec<u32> = Vec::new();
    let mut seen: HashSet<&str> = HashSet::new();
    let mut cursor = vec![0usize; queues.len()];
    loop {
        let mut added = false;
        for (q, queue) in queues.iter().enumerate() {
            if let Some(&index) = queue.get(cursor[q]) {
                cursor[q] += 1;
                added = true;
                if seen.insert(items[index as usize].key.as_str()) {
                    out.push(index);
                }
            }
        }
        if !added {
            break;
        }
    }
    out
}
