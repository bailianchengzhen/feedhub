//! 性能记录核心模块。
//!
//! 包含 `/proc` 解析、帧耗时统计与分位数、日志文件按天轮转与保留淘汰、
//! 请求计量与日志文案拼装、采样行与事件行的保序逐字节 JSON 组装与状态机。

pub mod files;
pub mod frame_stats;
pub mod meter;
pub mod proc;
pub mod recorder;

pub use files::{
    day_of_filename, day_stamp, PerfFileStore, PerfStatus, ANDROID_DIR_NAME, PERF_FILE_PREFIX,
    PERF_KEEP_DAYS,
};
pub use frame_stats::{frame_stats, round3, FrameStats, PERF_DEFAULT_DEADLINE_MS};
pub use meter::{request_failed_log_line, timeout_message, INTERACTIVE_REQUEST_TIMEOUT_MS};
pub use proc::{
    delta_or_null, parse_proc_stat, read_proc_self_stat, read_proc_self_threads,
    signed_delta_or_null, ticks_to_ms, top_thread_deltas, ProcTicks, ThreadCpu,
    PERF_CLOCK_TICKS_PER_SEC,
};
pub use recorder::{
    build_bg_span_line, build_event_comments_loaded, build_event_cover_window,
    build_event_detail_loaded, build_event_embed_batch, build_event_feed_refresh,
    build_event_first_frame, build_event_mark, build_event_request_failed, build_event_screen,
    build_event_video, build_sample_line, format_jvm_double, BatteryReading, FrameReading,
    GcReading, NetworkReading, PerfReading, PerfRecorderCore, PlaybackReading, ThermalReading,
};
