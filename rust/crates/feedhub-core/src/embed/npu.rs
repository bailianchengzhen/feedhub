//! **后端选择**（CPU / NPU 的判定）：选哪张图、现在算不算"可用"、什么时候能编图、回退成什么状态。
//!
//! `app/src/commonMain/kotlin/feedhub/app/embed/NpuBackend.kt` 的 Rust 版。
//! 这里只放**判定**，不放执行：探设备 / 建会话 / 编图 / 推理还是宿主（ONNX Runtime / QNN 的调用）。
//!
//! 三条口径一条没改：
//!
//! 1. **按批选图**：取"能装下这批最长那条（`max_tokens`）的**最短**那张"；不知道多长（`i32::MAX`）时
//!    **只能用包里最长那张**——拿短图算等于偷偷截断长文本，比多等一会儿糟得多；
//!    比最长那张还长时，最长那张就绪了就用它（截断），没就绪就等；
//! 2. **准备中不回退 CPU**：开关开着、还没就绪时批**等着**（不占 CPU、不丢、不写库）；只有真失败才回退；
//! 3. **编图的窗口**：只在前台、并且（在充电 或 手头没有批在算）时开始；等超过 2 分钟按"真失败"处理。
//!
//! 状态本身（`NpuStatus` 那一套）留在 Kotlin：它是 Compose 能观察到的界面状态，而且文案要跟着设置页走。

/// 一批能用的最短那张图的长度（`None` = 还没有能用的，继续等）。
///
/// - `ready`：已经就绪的图长度（升序）；
/// - `lengths`：包里带的图长度（升序、非空）；
/// - `max_tokens`：这批里最长那条的 token 数（`i32::MAX` = 不知道）。
pub fn pick_ready(ready: &[i32], lengths: &[i32], max_tokens: i32) -> Option<i32> {
    if ready.is_empty() || lengths.is_empty() {
        return None;
    }
    let longest = *lengths.last().unwrap();
    if let Some(len) = ready.iter().copied().find(|len| *len >= max_tokens) {
        return Some(len);
    }
    if max_tokens == i32::MAX {
        // 不知道多长：只能用包里最长那张（没就绪就等）
        return ready.last().copied().filter(|len| *len == longest);
    }
    // 比最长那张还长：最长那张就绪了就用它（会被截断，和以前一样）
    ready.last().copied().filter(|len| *len == longest)
}

/// 现在能不能开始编图：前台 + （在充电 或 没有批在算）。
pub fn can_compile_now(foreground: bool, charging: bool, in_flight: i32) -> bool {
    foreground && (charging || in_flight == 0)
}

/// 等"能编图"的窗口超时了没有。
pub fn compile_wait_exceeded(waited_ms: i64, max_wait_ms: i64) -> bool {
    waited_ms >= max_wait_ms
}

/// 这个组装现在有没有**能用的后端**（调度器据此决定"进不进队列"）。
///
/// - 有 CPU 模型：总能算（NPU 用不了也能退回 CPU）；
/// - 没有 CPU 模型（NPU 包）：只有开关打开、并且还没真失败时才算可用——准备中的批要进队列等着。
pub fn available(has_cpu: bool, npu_bundled: bool, requested: bool, broken: bool) -> bool {
    has_cpu || (npu_bundled && requested && !broken)
}

/// 写进 `item_embedding.model` 的标识：有就绪的 NPU 图 → NPU 的空间标识；没有 CPU 模型的包也用 NPU 的；
/// 否则用 CPU 的。
pub fn model_id(
    ready: &[i32],
    npu_bundled: bool,
    has_cpu: bool,
    npu_space_model: &str,
    cpu_model: &str,
) -> String {
    if !ready.is_empty() {
        return npu_space_model.to_string();
    }
    if !has_cpu && npu_bundled {
        return npu_space_model.to_string();
    }
    cpu_model.to_string()
}

