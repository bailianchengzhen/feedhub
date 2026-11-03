//! 性能记录的 UniFFI 导出。

use std::collections::HashMap;
use std::sync::Arc;

use feedhub_core::perf as core_perf;

// ===========================================================================
// `/proc` 解析与线程统计
// ===========================================================================

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiPerfProcTicks {
    pub pid: i32,
    pub name: String,
    pub utime_ticks: i64,
    pub stime_ticks: i64,
}

impl From<core_perf::ProcTicks> for FfiPerfProcTicks {
    fn from(t: core_perf::ProcTicks) -> Self {
        Self {
            pid: t.pid,
            name: t.name,
            utime_ticks: t.utime_ticks,
            stime_ticks: t.stime_ticks,
        }
    }
}

impl From<FfiPerfProcTicks> for core_perf::ProcTicks {
    fn from(t: FfiPerfProcTicks) -> Self {
        Self {
            pid: t.pid,
            name: t.name,
            utime_ticks: t.utime_ticks,
            stime_ticks: t.stime_ticks,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiPerfThreadCpu {
    pub name: String,
    pub ms: i64,
}

impl From<core_perf::ThreadCpu> for FfiPerfThreadCpu {
    fn from(t: core_perf::ThreadCpu) -> Self {
        Self {
            name: t.name,
            ms: t.ms,
        }
    }
}

#[uniffi::export]
pub fn perf_parse_proc_stat(text: String) -> Option<FfiPerfProcTicks> {
    core_perf::parse_proc_stat(&text).map(Into::into)
}

#[uniffi::export]
pub fn perf_top_thread_deltas(
    previous: Vec<FfiPerfProcTicks>,
    current: Vec<FfiPerfProcTicks>,
    limit: u32,
    ticks_per_sec: i64,
) -> Vec<FfiPerfThreadCpu> {
    let prev_map: HashMap<i32, core_perf::ProcTicks> =
        previous.into_iter().map(|t| (t.pid, t.into())).collect();
    let cur_vec: Vec<core_perf::ProcTicks> = current.into_iter().map(Into::into).collect();
    core_perf::top_thread_deltas(&prev_map, &cur_vec, limit as usize, ticks_per_sec)
        .into_iter()
        .map(Into::into)
        .collect()
}

#[uniffi::export]
pub fn perf_ticks_to_ms(ticks: i64, ticks_per_sec: i64) -> i64 {
    core_perf::ticks_to_ms(ticks, ticks_per_sec)
}

#[uniffi::export]
pub fn perf_delta_or_null(before: Option<i64>, now: Option<i64>) -> Option<i64> {
    core_perf::delta_or_null(before, now)
}

#[uniffi::export]
pub fn perf_signed_delta_or_null(before: Option<i64>, now: Option<i64>) -> Option<i64> {
    core_perf::signed_delta_or_null(before, now)
}

#[uniffi::export]
pub fn perf_read_proc_self_stat() -> Option<FfiPerfProcTicks> {
    core_perf::read_proc_self_stat().map(Into::into)
}

#[uniffi::export]
pub fn perf_read_proc_self_threads() -> Vec<FfiPerfProcTicks> {
    core_perf::read_proc_self_threads()
        .into_iter()
        .map(Into::into)
        .collect()
}

// ===========================================================================
// 帧统计分位数
// ===========================================================================

#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct FfiPerfFrameStats {
    pub frames: i32,
    pub janky: i32,
    pub p50: f64,
    pub p90: f64,
    pub p99: f64,
    pub max: f64,
    pub deadline_ms: f64,
}

impl From<core_perf::FrameStats> for FfiPerfFrameStats {
    fn from(s: core_perf::FrameStats) -> Self {
        Self {
            frames: s.frames,
            janky: s.janky,
            p50: s.p50,
            p90: s.p90,
            p99: s.p99,
            max: s.max,
            deadline_ms: s.deadline_ms,
        }
    }
}

#[uniffi::export]
pub fn perf_frame_stats(durations_ns: Vec<i64>, refresh_hz: f32) -> Option<FfiPerfFrameStats> {
    core_perf::frame_stats(&durations_ns, refresh_hz).map(Into::into)
}

#[uniffi::export]
pub fn perf_round3(v: f64) -> f64 {
    core_perf::round3(v)
}

// ===========================================================================
// 文件存储核心
// ===========================================================================

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiPerfStatus {
    pub dir_path: Option<String>,
    pub files: i32,
    pub bytes: i64,
}

#[derive(uniffi::Object)]
pub struct FfiPerfFileStore {
    inner: core_perf::PerfFileStore,
}

#[uniffi::export]
impl FfiPerfFileStore {
    #[uniffi::constructor]
    pub fn new(keep_days: u32) -> Arc<Self> {
        Arc::new(Self {
            inner: core_perf::PerfFileStore::new(keep_days as usize),
        })
    }

    pub fn append(
        &self,
        dir_path: Option<String>,
        line: String,
        now_ms: i64,
        offset_sec: i32,
        keep_from_offset_sec: i32,
    ) -> bool {
        self.inner.append(
            dir_path.as_deref(),
            &line,
            now_ms,
            offset_sec,
            keep_from_offset_sec,
        )
    }

    pub fn prune(&self, dir_path: Option<String>, now_ms: i64, keep_from_offset_sec: i32) -> u32 {
        self.inner
            .prune(dir_path.as_deref(), now_ms, keep_from_offset_sec)
    }

    pub fn files(&self, dir_path: Option<String>) -> Vec<String> {
        self.inner.files(dir_path.as_deref())
    }

    pub fn status(&self, dir_path: Option<String>) -> FfiPerfStatus {
        let st = self.inner.status(dir_path.as_deref());
        FfiPerfStatus {
            dir_path: st.dir_path,
            files: st.files,
            bytes: st.bytes,
        }
    }

    pub fn clear(&self, dir_path: Option<String>) -> u32 {
        self.inner.clear(dir_path.as_deref())
    }

    /// UniFFI Object 方法不能叫 close，所以命名为 shutdown
    pub fn shutdown(&self) {
        self.inner.shutdown();
    }

    pub fn last_error(&self) -> Option<String> {
        self.inner.last_error()
    }
}

// ===========================================================================
// 请求计量与文案
// ===========================================================================

#[uniffi::export]
pub fn perf_timeout_message(timeout_ms: i64) -> String {
    core_perf::timeout_message(timeout_ms)
}

#[uniffi::export]
pub fn perf_request_failed_log_line(
    kind: String,
    platform: String,
    reason: String,
    ms: i64,
    queue_ms: Option<i64>,
) -> String {
    core_perf::request_failed_log_line(&kind, &platform, &reason, ms, queue_ms)
}

// ===========================================================================
// 读数与 Recorder 状态机
// ===========================================================================

#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiPerfBatteryReading {
    pub current_ua: Option<i64>,
    pub current_unit: String,
    pub charge_uah: Option<i64>,
    pub level: Option<i32>,
    pub voltage_mv: Option<i32>,
    pub status: Option<String>,
    pub plugged: Option<String>,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiPerfNetworkReading {
    pub rx_bytes: i64,
    pub tx_bytes: i64,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiPerfThermalReading {
    pub status: Option<i32>,
    pub headroom: Option<f32>,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiPerfGcReading {
    pub count: Option<i64>,
    pub time_ms: Option<i64>,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiPerfFrameReading {
    pub durations_ns: Vec<i64>,
    pub refresh_hz: f32,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiPerfPlaybackReading {
    pub platform: String,
    pub playing: bool,
    pub hardware: Option<bool>,
    pub qn: Option<i32>,
    pub height: Option<i32>,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiPerfReading {
    pub battery: Option<FfiPerfBatteryReading>,
    pub cpu_ticks: Option<i64>,
    pub threads: Vec<FfiPerfProcTicks>,
    pub frames: Option<FfiPerfFrameReading>,
    pub network: Option<FfiPerfNetworkReading>,
    pub thermal: Option<FfiPerfThermalReading>,
    pub heap_used: Option<i64>,
    pub gc: Option<FfiPerfGcReading>,
    pub playback: Vec<FfiPerfPlaybackReading>,
    pub screen_on: Option<bool>,
}

fn from_ffi_reading(r: FfiPerfReading) -> core_perf::PerfReading {
    core_perf::PerfReading {
        battery: r.battery.map(|b| core_perf::BatteryReading {
            current_ua: b.current_ua,
            current_unit: b.current_unit,
            charge_uah: b.charge_uah,
            level: b.level,
            voltage_mv: b.voltage_mv,
            status: b.status,
            plugged: b.plugged,
        }),
        cpu_ticks: r.cpu_ticks,
        threads: r.threads.into_iter().map(Into::into).collect(),
        frames: r.frames.map(|f| core_perf::FrameReading {
            durations_ns: f.durations_ns,
            refresh_hz: f.refresh_hz,
        }),
        network: r.network.map(|n| core_perf::NetworkReading {
            rx_bytes: n.rx_bytes,
            tx_bytes: n.tx_bytes,
        }),
        thermal: r.thermal.map(|t| core_perf::ThermalReading {
            status: t.status,
            headroom: t.headroom,
        }),
        heap_used: r.heap_used,
        gc: r.gc.map(|g| core_perf::GcReading {
            count: g.count,
            time_ms: g.time_ms,
        }),
        playback: r
            .playback
            .into_iter()
            .map(|p| core_perf::PlaybackReading {
                platform: p.platform,
                playing: p.playing,
                hardware: p.hardware,
                qn: p.qn,
                height: p.height,
            })
            .collect(),
        screen_on: r.screen_on,
    }
}

#[derive(uniffi::Object)]
pub struct FfiPerfRecorderCore {
    inner: core_perf::PerfRecorderCore,
}

#[uniffi::export]
impl FfiPerfRecorderCore {
    #[uniffi::constructor]
    pub fn new(ticks_per_sec: i64) -> Arc<Self> {
        Arc::new(Self {
            inner: core_perf::PerfRecorderCore::new(ticks_per_sec),
        })
    }

    pub fn on_enabled(&self, reading: FfiPerfReading, now_ms: i64, screen: String) -> String {
        self.inner
            .on_enabled(from_ffi_reading(reading), now_ms, &screen)
    }

    pub fn on_disabled(&self, now_ms: i64, screen: String) -> String {
        self.inner.on_disabled(now_ms, &screen)
    }

    pub fn begin_background(
        &self,
        reading: FfiPerfReading,
        now_ms: i64,
        screen: String,
    ) -> String {
        self.inner
            .begin_background(from_ffi_reading(reading), now_ms, &screen)
    }

    pub fn finish_background(&self, end_reading: FfiPerfReading, now_ms: i64) -> Option<String> {
        self.inner
            .finish_background(from_ffi_reading(end_reading), now_ms)
    }

    pub fn sample_once(
        &self,
        reading: FfiPerfReading,
        self_cpu_ms: i64,
        now_ms: i64,
        screen: String,
    ) -> String {
        self.inner
            .sample_once(from_ffi_reading(reading), self_cpu_ms, now_ms, &screen)
    }
}

// ===========================================================================
// 事件行独立生成
// ===========================================================================

#[uniffi::export]
pub fn perf_build_event_mark(now_ms: i64, screen: String, name: String) -> String {
    core_perf::build_event_mark(now_ms, &screen, &name)
}

#[uniffi::export]
pub fn perf_build_event_feed_refresh(
    now_ms: i64,
    screen: String,
    kind: String,
    start: bool,
    platforms: Vec<String>,
    count: Option<i32>,
    d_ms: Option<i64>,
) -> String {
    core_perf::build_event_feed_refresh(
        now_ms, &screen, &kind, start, &platforms, count, d_ms,
    )
}

#[uniffi::export]
pub fn perf_build_event_detail_loaded(
    now_ms: i64,
    screen: String,
    platform: String,
    kind: String,
    d_ms: i64,
    queue_ms: Option<i64>,
) -> String {
    core_perf::build_event_detail_loaded(now_ms, &screen, &platform, &kind, d_ms, queue_ms)
}

#[uniffi::export]
pub fn perf_build_event_comments_loaded(
    now_ms: i64,
    screen: String,
    platform: String,
    count: i32,
    d_ms: i64,
    queue_ms: Option<i64>,
) -> String {
    core_perf::build_event_comments_loaded(now_ms, &screen, &platform, count, d_ms, queue_ms)
}

#[uniffi::export]
pub fn perf_build_event_request_failed(
    now_ms: i64,
    screen: String,
    kind: String,
    platform: String,
    reason: String,
    d_ms: i64,
    queue_ms: Option<i64>,
) -> String {
    core_perf::build_event_request_failed(
        now_ms, &screen, &kind, &platform, &reason, d_ms, queue_ms,
    )
}

#[uniffi::export]
pub fn perf_build_event_embed_batch(
    now_ms: i64,
    screen: String,
    backend: String,
    count: i32,
    d_ms: i64,
    model: String,
    len: i32,
) -> String {
    core_perf::build_event_embed_batch(now_ms, &screen, &backend, count, d_ms, &model, len)
}

#[uniffi::export]
pub fn perf_build_event_cover_window(
    now_ms: i64,
    screen: String,
    count: i32,
    vision_ms: i64,
    text_ms: i64,
    total_ms: i64,
    compiled: i32,
) -> String {
    core_perf::build_event_cover_window(
        now_ms, screen.as_str(), count, vision_ms, text_ms, total_ms, compiled,
    )
}

#[uniffi::export]
pub fn perf_build_event_video(
    now_ms: i64,
    screen: String,
    platform: String,
    action: String,
    playing: bool,
    hardware: Option<bool>,
    qn: Option<i32>,
    height: Option<i32>,
    d_ms: Option<i64>,
) -> String {
    core_perf::build_event_video(
        now_ms, &screen, &platform, &action, playing, hardware, qn, height, d_ms,
    )
}

#[uniffi::export]
pub fn perf_build_event_first_frame(now_ms: i64, screen: String, d_ms: i64) -> String {
    core_perf::build_event_first_frame(now_ms, &screen, d_ms)
}

#[uniffi::export]
pub fn perf_build_event_screen(now_ms: i64, screen: String, lit: bool) -> String {
    core_perf::build_event_screen(now_ms, &screen, lit)
}
