//! **桌面播放引擎**：GStreamer 1.26 + VA-API 硬解，`gstreamer-rs` 版。
//!
//! 这一层原来全在 Kotlin 的 `app/src/desktopMain/.../video/GstPlayerEngine.kt` +
//! `GstRuntime.kt`（gst1-java-core + JNA）。
//! 宿主只剩：
//!
//! 当前不碰 |
//!
//! 引擎本身（建管线 / 总线 / 状态机 / seek / 倍速 / 循环 / 缓冲 / 硬解退软解 / 备用地址 /
//! 帧节奏记账 / souphttpsrc 的代理与请求头）**全在这里**。
//!
//! ## 出帧：共享直接内存，不是每帧过 UniFFI
//!
//! appsink 拿到的 BGRA 帧直接写进一块 `mmap` 出来的**共享内存**（双缓冲 + 序号），Kotlin 侧
//! 只读那块内存自己造 Skia 的 `Image`。过边界的只有"新帧序号变了"这一个整数
//! （[`crate::ffi::FfiGstEngine::frame_seq`]），**一帧的像素一个字节都不过 FFI**。
//! 三条缓冲轮转（appsink 那边上游还在写下一帧时，界面这边可能正在读上一帧）。
//!
//! ## 管线（一次 `open` 一条）
//!
//! ```text
//! uridecodebin3 ─┬─ video/x-raw(VAMemory) ─ vapostproc ─ capsfilter(BGRA[,宽×高]) ─ appsink
//!   (souphttpsrc  │
//!    + dashdemux2 └─ audio/x-raw ─ audioconvert ─ scaletempo ─ audioconvert ─ audioresample
//!    + decodebin3)                ─ volume ─ autoaudiosink（无窗口 / 测量时 fakesink）
//! ```
//!
//! - **解码**：decodebin3 按 rank 选；va 的 h264 / h265 / av1 解码器排在软解 `avdec_*` 前面；
//!   nvcodec 全降到 NONE（不唤醒独显，见 [`runtime`]）。vapostproc 在 GPU 上转 BGRA 并按画布
//!   像素缩放，下载到内存的只有屏幕上要显示的那些像素。
//! - **拖动**：整条管线 flush seek；续播（`open` 的 `start_ms`）预卷完成后先 seek 再播。
//! - **倍速**：`INSTANT_RATE_CHANGE` 只改速率、不清管线；用不了才回退成带新速率的 flush seek，
//!   而且回退也装成"什么都没发生"（不转圈、不冻进度、画面停最后一帧）。
//! - **循环**：EOS 时同一条管线 flush seek 回开头接着播，不重建。
//! - **出错**：硬解元素出错退回软解重来；否则换下一个备用地址；都不行才报给界面。
//!
//! 每条口径的来历（为什么对齐宽度是 64、为什么要等视频支路接上再算预卷完、为什么用系统时钟
//! 而不是声卡时钟）都照原 Kotlin 的注释搬过来了，在各个函数头上。

pub mod engine;
pub mod ffi;
pub mod runtime;
pub mod surface;
pub mod vaapi;

uniffi::setup_scaffolding!();

/// 一帧最多能有多大（宽 × 高 × 4 字节的上限检查用）。4K 是 3840×2160×4 = 33 MB。
/// 共享内存按"源尺寸"一次性 mmap；超过这个数说明地址不对（拒绝，不去 mmap 一个天文数字）。
pub const MAX_FRAME_BYTES: usize = 8192 * 8192 * 4;

/// 共享内存里一轮缓冲的槽数。appsink 写第 N+1 帧时界面可能正在读第 N 帧，
/// 而第 N 帧被换掉之前界面还没来读的情况也要能容纳（**三槽**：写、待读、正在读）。
pub const SURFACE_SLOTS: usize = 3;
