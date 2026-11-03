//! 性能记录采样行与事件行的 JSON 生成及状态机。

use std::collections::HashMap;
use std::sync::Mutex;

use crate::perf::frame_stats::{frame_stats, round3};
use crate::perf::proc::{
    delta_or_null, signed_delta_or_null, ticks_to_ms, top_thread_deltas, ProcTicks,
    PERF_CLOCK_TICKS_PER_SEC,
};

/// JVM Double.toString 风格格式化（整数补 `.0`，避免科学计数法或无小数点的输出）。
pub fn format_jvm_double(v: f64) -> String {
    let v = if v == 0.0 { 0.0 } else { v };
    let s = format!("{v}");
    if !s.contains('.') && !s.contains('e') && !s.contains('E') {
        format!("{s}.0")
    } else {
        s
    }
}

// ---------------------------------------------------------------------------
// 读数模型
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, PartialEq)]
pub struct BatteryReading {
    pub current_ua: Option<i64>,
    pub current_unit: String,
    pub charge_uah: Option<i64>,
    pub level: Option<i32>,
    pub voltage_mv: Option<i32>,
    pub status: Option<String>,
    pub plugged: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetworkReading {
    pub rx_bytes: i64,
    pub tx_bytes: i64,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ThermalReading {
    pub status: Option<i32>,
    pub headroom: Option<f32>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GcReading {
    pub count: Option<i64>,
    pub time_ms: Option<i64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FrameReading {
    pub durations_ns: Vec<i64>,
    pub refresh_hz: f32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaybackReading {
    pub platform: String,
    pub playing: bool,
    pub hardware: Option<bool>,
    pub qn: Option<i32>,
    pub height: Option<i32>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct PerfReading {
    pub battery: Option<BatteryReading>,
    pub cpu_ticks: Option<i64>,
    pub threads: Vec<ProcTicks>,
    pub frames: Option<FrameReading>,
    pub network: Option<NetworkReading>,
    pub thermal: Option<ThermalReading>,
    pub heap_used: Option<i64>,
    pub gc: Option<GcReading>,
    pub playback: Vec<PlaybackReading>,
    pub screen_on: Option<bool>,
}

// ---------------------------------------------------------------------------
// JSON 序列化辅助
// ---------------------------------------------------------------------------

fn append_json_str(out: &mut String, s: &str) {
    crate::hosted::push_json_string(out, s);
}

// ---------------------------------------------------------------------------
// 采样行与事件行构造
// ---------------------------------------------------------------------------

pub fn build_sample_line(
    now_ms: i64,
    screen: &str,
    previous: Option<&PerfReading>,
    reading: &PerfReading,
    self_cpu_ms: i64,
    ticks_per_sec: i64,
) -> String {
    let proc_ms = delta_or_null(
        previous.and_then(|p| p.cpu_ticks),
        reading.cpu_ticks,
    )
    .map(|d| ticks_to_ms(d, ticks_per_sec));

    let threads = if let Some(prev) = previous {
        let prev_map: HashMap<i32, ProcTicks> =
            prev.threads.iter().map(|t| (t.pid, t.clone())).collect();
        top_thread_deltas(&prev_map, &reading.threads, 8, ticks_per_sec)
    } else {
        Vec::new()
    };

    let stats = reading
        .frames
        .as_ref()
        .and_then(|f| frame_stats(&f.durations_ns, f.refresh_hz));
    let frames_seen = reading
        .frames
        .as_ref()
        .map(|f| f.durations_ns.iter().filter(|&&d| d > 0).count());

    let gc_count = delta_or_null(
        previous.and_then(|p| p.gc.as_ref().and_then(|g| g.count)),
        reading.gc.as_ref().and_then(|g| g.count),
    );
    let gc_ms = delta_or_null(
        previous.and_then(|p| p.gc.as_ref().and_then(|g| g.time_ms)),
        reading.gc.as_ref().and_then(|g| g.time_ms),
    );

    let mut out = String::with_capacity(512);
    out.push_str(r#"{"type":"sample","ts":"#);
    out.push_str(&now_ms.to_string());
    out.push_str(r#","screen":"#);
    append_json_str(&mut out, screen);

    // 1. battery
    if let Some(b) = &reading.battery {
        out.push_str(r#","battery":{"#);
        let mut first = true;
        if let Some(ua) = b.current_ua {
            out.push_str(r#""current_ua":"#);
            out.push_str(&ua.to_string());
            out.push_str(r#","current_unit":"#);
            append_json_str(&mut out, &b.current_unit);
            first = false;
        }
        if let Some(ch) = b.charge_uah {
            if !first {
                out.push(',');
            }
            out.push_str(r#""charge_uah":"#);
            out.push_str(&ch.to_string());
            first = false;
        }
        if let Some(lvl) = b.level {
            if !first {
                out.push(',');
            }
            out.push_str(r#""level":"#);
            out.push_str(&lvl.to_string());
            first = false;
        }
        if let Some(v) = b.voltage_mv {
            if !first {
                out.push(',');
            }
            out.push_str(r#""voltage_mv":"#);
            out.push_str(&v.to_string());
            first = false;
        }
        if let Some(st) = &b.status {
            if !first {
                out.push(',');
            }
            out.push_str(r#""status":"#);
            append_json_str(&mut out, st);
            first = false;
        }
        if let Some(pl) = &b.plugged {
            if !first {
                out.push(',');
            }
            out.push_str(r#""plugged":"#);
            append_json_str(&mut out, pl);
        }
        out.push('}');
    }

    // 2. screen_on
    if let Some(so) = reading.screen_on {
        out.push_str(r#","screen_on":"#);
        out.push_str(if so { "true" } else { "false" });
    }

    // 3. cpu
    if proc_ms.is_some() || !threads.is_empty() || self_cpu_ms >= 0 {
        out.push_str(r#","cpu":{"#);
        let mut first = true;
        if let Some(ms) = proc_ms {
            out.push_str(r#""proc_ms":"#);
            out.push_str(&ms.to_string());
            first = false;
        }
        if !threads.is_empty() {
            if !first {
                out.push(',');
            }
            out.push_str(r#""threads":["#);
            for (ti, t) in threads.iter().enumerate() {
                if ti > 0 {
                    out.push(',');
                }
                out.push_str(r#"{"t":"#);
                append_json_str(&mut out, &t.name);
                out.push_str(r#","ms":"#);
                out.push_str(&t.ms.to_string());
                out.push('}');
            }
            out.push(']');
            first = false;
        }
        if !first {
            out.push(',');
        }
        out.push_str(r#""sampler_ms":"#);
        out.push_str(&self_cpu_ms.to_string());
        out.push('}');
    }

    // 4. frames
    if reading.frames.is_some() {
        out.push_str(r#","frames":{"n":"#);
        out.push_str(&frames_seen.unwrap_or(0).to_string());
        if let Some(st) = stats {
            out.push_str(r#","janky":"#);
            out.push_str(&st.janky.to_string());
            out.push_str(r#","p50_ms":"#);
            out.push_str(&format_jvm_double(st.p50));
            out.push_str(r#","p90_ms":"#);
            out.push_str(&format_jvm_double(st.p90));
            out.push_str(r#","p99_ms":"#);
            out.push_str(&format_jvm_double(st.p99));
            out.push_str(r#","max_ms":"#);
            out.push_str(&format_jvm_double(st.max));
            out.push_str(r#","deadline_ms":"#);
            out.push_str(&format_jvm_double(st.deadline_ms));
        }
        out.push('}');
    }

    // 5. net
    let rx_opt = delta_or_null(
        previous.and_then(|p| p.network.as_ref().map(|n| n.rx_bytes)),
        reading.network.as_ref().map(|n| n.rx_bytes),
    );
    if let Some(rx) = rx_opt {
        out.push_str(r#","net":{"rx":"#);
        out.push_str(&rx.to_string());
        let tx_opt = delta_or_null(
            previous.and_then(|p| p.network.as_ref().map(|n| n.tx_bytes)),
            reading.network.as_ref().map(|n| n.tx_bytes),
        );
        if let Some(tx) = tx_opt {
            out.push_str(r#","tx":"#);
            out.push_str(&tx.to_string());
        }
        out.push('}');
    }

    // 6. thermal
    if let Some(t) = &reading.thermal {
        out.push_str(r#","thermal":{"#);
        let mut first = true;
        if let Some(st) = t.status {
            out.push_str(r#""status":"#);
            out.push_str(&st.to_string());
            first = false;
        }
        if let Some(hr) = t.headroom {
            if hr.is_finite() {
                if !first {
                    out.push(',');
                }
                out.push_str(r#""headroom":"#);
                out.push_str(&format_jvm_double(round3(hr as f64)));
            }
        }
        out.push('}');
    }

    // 7. mem
    if reading.heap_used.is_some() || gc_count.is_some() || gc_ms.is_some() {
        out.push_str(r#","mem":{"#);
        let mut first = true;
        if let Some(h) = reading.heap_used {
            out.push_str(r#""heap":"#);
            out.push_str(&h.to_string());
            first = false;
        }
        if let Some(c) = gc_count {
            if !first {
                out.push(',');
            }
            out.push_str(r#""gc_count":"#);
            out.push_str(&c.to_string());
            first = false;
        }
        if let Some(m) = gc_ms {
            if !first {
                out.push(',');
            }
            out.push_str(r#""gc_ms":"#);
            out.push_str(&m.to_string());
        }
        out.push('}');
    }

    // 8. play
    if !reading.playback.is_empty() {
        out.push_str(r#","play":["#);
        for (i, p) in reading.playback.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            out.push_str(r#"{"p":"#);
            append_json_str(&mut out, &p.platform);
            out.push_str(r#","on":"#);
            out.push_str(if p.playing { "true" } else { "false" });
            if let Some(hw) = p.hardware {
                out.push_str(r#","hw":"#);
                out.push_str(if hw { "true" } else { "false" });
            }
            if let Some(qn) = p.qn {
                out.push_str(r#","qn":"#);
                out.push_str(&qn.to_string());
            }
            if let Some(h) = p.height {
                out.push_str(r#","h":"#);
                out.push_str(&h.to_string());
            }
            out.push('}');
        }
        out.push(']');
    }

    // 9. playing
    out.push_str(r#","playing":"#);
    out.push_str(if reading.playback.iter().any(|p| p.playing) {
        "true"
    } else {
        "false"
    });

    out.push('}');
    out
}

pub fn build_bg_span_line(
    end_at: i64,
    at_screen: &str,
    dur_ms: i64,
    cpu_ms: Option<i64>,
    charge_uah: Option<i64>,
    rx: Option<i64>,
    tx: Option<i64>,
) -> String {
    let mut out = String::with_capacity(256);
    out.push_str(r#"{"type":"bg_span","ts":"#);
    out.push_str(&end_at.to_string());
    out.push_str(r#","screen":"#);
    append_json_str(&mut out, at_screen);
    out.push_str(r#","dur_ms":"#);
    out.push_str(&dur_ms.to_string());
    if let Some(c) = cpu_ms {
        out.push_str(r#","cpu_ms":"#);
        out.push_str(&c.to_string());
    }
    if let Some(ch) = charge_uah {
        out.push_str(r#","charge_uah":"#);
        out.push_str(&ch.to_string());
    }
    if let Some(r) = rx {
        out.push_str(r#","rx":"#);
        out.push_str(&r.to_string());
    }
    if let Some(t) = tx {
        out.push_str(r#","tx":"#);
        out.push_str(&t.to_string());
    }
    out.push('}');
    out
}

pub fn build_event_mark(now_ms: i64, screen: &str, name: &str) -> String {
    let mut out = String::with_capacity(128);
    out.push_str(r#"{"type":"event","ts":"#);
    out.push_str(&now_ms.to_string());
    out.push_str(r#","screen":"#);
    append_json_str(&mut out, screen);
    out.push_str(r#","name":"#);
    append_json_str(&mut out, name);
    out.push('}');
    out
}

pub fn build_event_feed_refresh(
    now_ms: i64,
    screen: &str,
    kind: &str,
    start: bool,
    platforms: &[String],
    count: Option<i32>,
    d_ms: Option<i64>,
) -> String {
    let mut out = String::with_capacity(256);
    out.push_str(r#"{"type":"event","ts":"#);
    out.push_str(&now_ms.to_string());
    out.push_str(r#","screen":"#);
    append_json_str(&mut out, screen);
    out.push_str(r#","name":"feed_refresh","f":{"kind":"#);
    append_json_str(&mut out, kind);
    out.push_str(r#","phase":"#);
    append_json_str(&mut out, if start { "start" } else { "end" });
    out.push_str(r#","platforms":["#);
    for (i, p) in platforms.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        append_json_str(&mut out, p);
    }
    out.push(']');
    if let Some(c) = count {
        out.push_str(r#","count":"#);
        out.push_str(&c.to_string());
    }
    if let Some(d) = d_ms {
        out.push_str(r#","d_ms":"#);
        out.push_str(&d.to_string());
    }
    out.push_str(r#"}}"#);
    out
}

pub fn build_event_detail_loaded(
    now_ms: i64,
    screen: &str,
    platform: &str,
    kind: &str,
    d_ms: i64,
    queue_ms: Option<i64>,
) -> String {
    let mut out = String::with_capacity(192);
    out.push_str(r#"{"type":"event","ts":"#);
    out.push_str(&now_ms.to_string());
    out.push_str(r#","screen":"#);
    append_json_str(&mut out, screen);
    out.push_str(r#","name":"detail","f":{"platform":"#);
    append_json_str(&mut out, platform);
    out.push_str(r#","kind":"#);
    append_json_str(&mut out, kind);
    out.push_str(r#","d_ms":"#);
    out.push_str(&d_ms.to_string());
    if let Some(q) = queue_ms {
        out.push_str(r#","queue_ms":"#);
        out.push_str(&q.to_string());
    }
    out.push_str(r#"}}"#);
    out
}

pub fn build_event_comments_loaded(
    now_ms: i64,
    screen: &str,
    platform: &str,
    count: i32,
    d_ms: i64,
    queue_ms: Option<i64>,
) -> String {
    let mut out = String::with_capacity(192);
    out.push_str(r#"{"type":"event","ts":"#);
    out.push_str(&now_ms.to_string());
    out.push_str(r#","screen":"#);
    append_json_str(&mut out, screen);
    out.push_str(r#","name":"comments","f":{"platform":"#);
    append_json_str(&mut out, platform);
    out.push_str(r#","count":"#);
    out.push_str(&count.to_string());
    out.push_str(r#","d_ms":"#);
    out.push_str(&d_ms.to_string());
    if let Some(q) = queue_ms {
        out.push_str(r#","queue_ms":"#);
        out.push_str(&q.to_string());
    }
    out.push_str(r#"}}"#);
    out
}

pub fn build_event_request_failed(
    now_ms: i64,
    screen: &str,
    kind: &str,
    platform: &str,
    reason: &str,
    d_ms: i64,
    queue_ms: Option<i64>,
) -> String {
    let mut out = String::with_capacity(256);
    out.push_str(r#"{"type":"event","ts":"#);
    out.push_str(&now_ms.to_string());
    out.push_str(r#","screen":"#);
    append_json_str(&mut out, screen);
    out.push_str(r#","name":"#);
    let event_name = format!("{kind}_failed");
    append_json_str(&mut out, &event_name);
    out.push_str(r#","f":{"platform":"#);
    append_json_str(&mut out, platform);
    out.push_str(r#","reason":"#);
    append_json_str(&mut out, reason);
    out.push_str(r#","d_ms":"#);
    out.push_str(&d_ms.to_string());
    if let Some(q) = queue_ms {
        out.push_str(r#","queue_ms":"#);
        out.push_str(&q.to_string());
    }
    out.push_str(r#"}}"#);
    out
}

pub fn build_event_embed_batch(
    now_ms: i64,
    screen: &str,
    backend: &str,
    count: i32,
    d_ms: i64,
    model: &str,
    len: i32,
) -> String {
    let mut out = String::with_capacity(256);
    out.push_str(r#"{"type":"event","ts":"#);
    out.push_str(&now_ms.to_string());
    out.push_str(r#","screen":"#);
    append_json_str(&mut out, screen);
    out.push_str(r#","name":"embed_batch","f":{"backend":"#);
    append_json_str(&mut out, backend);
    out.push_str(r#","count":"#);
    out.push_str(&count.to_string());
    out.push_str(r#","d_ms":"#);
    out.push_str(&d_ms.to_string());
    out.push_str(r#","model":"#);
    append_json_str(&mut out, model);
    if len > 0 {
        out.push_str(r#","len":"#);
        out.push_str(&len.to_string());
    }
    out.push_str(r#"}}"#);
    out
}

pub fn build_event_cover_window(
    now_ms: i64,
    screen: &str,
    count: i32,
    vision_ms: i64,
    text_ms: i64,
    total_ms: i64,
    compiled: i32,
) -> String {
    let mut out = String::with_capacity(256);
    out.push_str(r#"{"type":"event","ts":"#);
    out.push_str(&now_ms.to_string());
    out.push_str(r#","screen":"#);
    append_json_str(&mut out, screen);
    out.push_str(r#","name":"cover_window","f":{"count":"#);
    out.push_str(&count.to_string());
    out.push_str(r#","vision_ms":"#);
    out.push_str(&vision_ms.to_string());
    out.push_str(r#","text_ms":"#);
    out.push_str(&text_ms.to_string());
    out.push_str(r#","d_ms":"#);
    out.push_str(&total_ms.to_string());
    out.push_str(r#","compiled":"#);
    out.push_str(&compiled.to_string());
    out.push_str(r#"}}"#);
    out
}

pub fn build_event_video(
    now_ms: i64,
    screen: &str,
    platform: &str,
    action: &str,
    playing: bool,
    hardware: Option<bool>,
    qn: Option<i32>,
    height: Option<i32>,
    d_ms: Option<i64>,
) -> String {
    let mut out = String::with_capacity(256);
    out.push_str(r#"{"type":"event","ts":"#);
    out.push_str(&now_ms.to_string());
    out.push_str(r#","screen":"#);
    append_json_str(&mut out, screen);
    out.push_str(r#","name":"video","f":{"platform":"#);
    append_json_str(&mut out, platform);
    out.push_str(r#","action":"#);
    append_json_str(&mut out, action);
    out.push_str(r#","playing":"#);
    out.push_str(if playing { "true" } else { "false" });
    if let Some(hw) = hardware {
        out.push_str(r#","hw":"#);
        out.push_str(if hw { "true" } else { "false" });
    }
    if let Some(q) = qn {
        out.push_str(r#","qn":"#);
        out.push_str(&q.to_string());
    }
    if let Some(h) = height {
        out.push_str(r#","h":"#);
        out.push_str(&h.to_string());
    }
    if let Some(d) = d_ms {
        out.push_str(r#","d_ms":"#);
        out.push_str(&d.to_string());
    }
    out.push_str(r#"}}"#);
    out
}

pub fn build_event_first_frame(now_ms: i64, screen: &str, d_ms: i64) -> String {
    let mut out = String::with_capacity(128);
    out.push_str(r#"{"type":"event","ts":"#);
    out.push_str(&now_ms.to_string());
    out.push_str(r#","screen":"#);
    append_json_str(&mut out, screen);
    out.push_str(r#","name":"first_frame","f":{"d_ms":"#);
    out.push_str(&d_ms.to_string());
    out.push_str(r#"}}"#);
    out
}

pub fn build_event_screen(now_ms: i64, screen: &str, lit: bool) -> String {
    build_event_mark(
        now_ms,
        screen,
        if lit { "screen_on" } else { "screen_off" },
    )
}

// ---------------------------------------------------------------------------
// 状态机核心
// ---------------------------------------------------------------------------

struct CoreInner {
    last: Option<PerfReading>,
    bg_base: Option<PerfReading>,
    bg_at: i64,
    bg_screen: String,
    ticks_per_sec: i64,
}

/// 性能记录器的状态记忆与计算核心。
pub struct PerfRecorderCore {
    inner: Mutex<CoreInner>,
}

impl PerfRecorderCore {
    pub fn new(ticks_per_sec: i64) -> Self {
        Self {
            inner: Mutex::new(CoreInner {
                last: None,
                bg_base: None,
                bg_at: 0,
                bg_screen: String::new(),
                ticks_per_sec: if ticks_per_sec <= 0 {
                    PERF_CLOCK_TICKS_PER_SEC
                } else {
                    ticks_per_sec
                },
            }),
        }
    }

    /// 开关打开：初始化基线，返回 start 事件行。
    pub fn on_enabled(&self, reading: PerfReading, now_ms: i64, screen: &str) -> String {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.last = Some(reading);
        build_event_mark(now_ms, screen, "start")
    }

    /// 开关关闭：清除基线，返回 stop 事件行。
    pub fn on_disabled(&self, now_ms: i64, screen: &str) -> String {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.last = None;
        inner.bg_base = None;
        build_event_mark(now_ms, screen, "stop")
    }

    /// 进后台：记录基线，返回 background 事件行。
    pub fn begin_background(&self, reading: PerfReading, now_ms: i64, screen: &str) -> String {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.bg_base = Some(reading);
        inner.bg_at = now_ms;
        inner.bg_screen = screen.to_string();
        build_event_mark(now_ms, screen, "background")
    }

    /// 回前台：结算后台 span，返回 bg_span 行（如果没有进后台基线则返回 None）。
    pub fn finish_background(&self, end_reading: PerfReading, now_ms: i64) -> Option<String> {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let from = inner.bg_base.take()?;
        let start_at = inner.bg_at;
        let at_screen = std::mem::take(&mut inner.bg_screen);
        let ticks_per_sec = inner.ticks_per_sec;

        let cpu_ms = delta_or_null(from.cpu_ticks, end_reading.cpu_ticks)
            .map(|d| ticks_to_ms(d, ticks_per_sec));
        let charge = signed_delta_or_null(
            from.battery.as_ref().and_then(|b| b.charge_uah),
            end_reading.battery.as_ref().and_then(|b| b.charge_uah),
        );
        let rx = delta_or_null(
            from.network.as_ref().map(|n| n.rx_bytes),
            end_reading.network.as_ref().map(|n| n.rx_bytes),
        );
        let tx = delta_or_null(
            from.network.as_ref().map(|n| n.tx_bytes),
            end_reading.network.as_ref().map(|n| n.tx_bytes),
        );
        let dur_ms = now_ms.saturating_sub(start_at).max(0);

        Some(build_bg_span_line(
            now_ms,
            &at_screen,
            dur_ms,
            cpu_ms,
            charge,
            rx,
            tx,
        ))
    }

    /// 采样一次：计算增量与分位数，更新内部 last，返回 sample 行。
    pub fn sample_once(
        &self,
        reading: PerfReading,
        self_cpu_ms: i64,
        now_ms: i64,
        screen: &str,
    ) -> String {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let previous = inner.last.take();
        let line = build_sample_line(
            now_ms,
            screen,
            previous.as_ref(),
            &reading,
            self_cpu_ms,
            inner.ticks_per_sec,
        );
        inner.last = Some(reading);
        line
    }
}

