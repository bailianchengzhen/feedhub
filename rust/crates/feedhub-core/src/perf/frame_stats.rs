//! 帧耗时统计与分位数计算。

/// 判定卡顿用的兜底截止时间（毫秒）：不知道刷新率时按 60Hz 算。
pub const PERF_DEFAULT_DEADLINE_MS: f64 = 1000.0 / 60.0;

/// 帧耗时统计。
#[derive(Debug, Clone, PartialEq)]
pub struct FrameStats {
    /// 这一段里交了多少帧。
    pub frames: i32,
    /// 超过截止时间（一个刷新周期）的帧数。
    pub janky: i32,
    pub p50: f64,
    pub p90: f64,
    pub p99: f64,
    pub max: f64,
    /// 判定卡顿用的截止时间（毫秒）= 1 秒 / 刷新率。
    pub deadline_ms: f64,
}

impl FrameStats {
    pub fn jank_rate(&self) -> f64 {
        if self.frames == 0 {
            0.0
        } else {
            self.janky as f64 / self.frames as f64
        }
    }
}

/// 保留三位小数（毫秒），IEEE 754 银行家舍入（round half to even），与 Kotlin `kotlin.math.round` 完全一致。
pub fn round3(v: f64) -> f64 {
    (v * 1000.0).round_ties_even() / 1000.0
}

/// 按帧耗时算统计。耗时 <= 0 的帧丢掉，不算进总帧数。
///
/// 注意 `refresh_hz`：`f32` 转 `f64` 的精度差异与 Kotlin `(refreshHz: Float).toDouble()` 完全一致。
pub fn frame_stats(durations_ns: &[i64], refresh_hz: f32) -> Option<FrameStats> {
    let mut valid: Vec<i64> = durations_ns.iter().copied().filter(|&x| x > 0).collect();
    if valid.is_empty() {
        return None;
    }
    valid.sort_unstable();

    let hz = if refresh_hz > 0.0 {
        refresh_hz as f64
    } else {
        60.0
    };
    let deadline_ms = 1000.0 / hz;
    let deadline_ns = (1_000_000_000.0 / hz) as i64;
    let n = valid.len();

    let percentile = |p: f64| -> f64 {
        let rank = ((p * n as f64).ceil() as usize).clamp(1, n);
        valid[rank - 1] as f64 / 1_000_000.0
    };

    let janky = valid.iter().filter(|&&x| x > deadline_ns).count() as i32;

    Some(FrameStats {
        frames: n as i32,
        janky,
        p50: round3(percentile(0.50)),
        p90: round3(percentile(0.90)),
        p99: round3(percentile(0.99)),
        max: round3(*valid.last().unwrap() as f64 / 1_000_000.0),
        deadline_ms: round3(deadline_ms),
    })
}

