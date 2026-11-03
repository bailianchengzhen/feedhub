//! **播放器决策层**：选流 / 解码能力 / 弹幕 / 清单改写 / 播放状态机。
//!
//! 这一层原来全在 Kotlin 的 `app/.../video/` 里（`StreamSelection` / `DecodeCapability` /
//! `DecodeLedger` / `DanmakuFilter` / `DanmakuModel` / `DanmakuSettings` / `PlaybackSession` 的状态机 /
//! `ManifestRewrite` / `PlayerProxy`）。
//! **决策 / 计算 / 状态机 / 落盘格式 / 解析生成**都在这里，宿主只剩：
//!
//! | 留在宿主 | 为什么 |
//! | GStreamer / Media3 / libvlc 的引擎调用、每一帧的绘制 | 调操作系统 / 框架接口 |
//! | 中转服务器的 socket（`com.sun.net.httpserver`）与 OkHttp 收发 | 真正把请求发出去 |
//! | 弹幕文字的宽度测量 | 要用 Compose `TextMeasurer` / `android.graphics.Paint`，那是渲染 |
//! | 协程 / 定时器 / 前后台事件 | 触发 |
//! | 时区 / 地区的日期串 | 读系统时区是操作系统的事 |
//!
//! 一条贯穿全层的约定：**过 FFI 的东西按"一批"走**（一段弹幕、一整份清单、一次选流），
//! 不逐条 / 逐帧过边界。弹幕的排布是"进时间线时一条一次"，每帧只剩"按已排好的 startMs /
//! durationMs 算 x"，那一步是纯算术、留在宿主（见 [`danmaku::scheduler`] 的说明）。
//!
//! 子模块：
//! - [`select`]：选哪一路流 / 哪一档清晰度（按解码能力排序）、编码 / 倍速的文字；
//! - [`ledger`]：解码能力账本（测量规则、只降不升、TTL、环境指纹、落盘格式）；
//! - [`danmaku`]：过滤 / 合并 / 时间线 / 装载策略 / 排布 / 设置换算；
//! - [`manifest`]：DASH 清单生成 + DASH / HLS 清单改写；
//! - [`relay`]：中转地址映射（登记顺序、扩展名、模板占位符、`Range` 解析）；
//! - [`session`]：播放状态机（倍速 / 长按锁定 / 拖动 / 结束 / 后台续播 / 取流失败分类）。

pub mod danmaku;
pub mod ledger;
pub mod manifest;
pub mod relay;
pub mod select;
pub mod session;

pub use ledger::{DecodeCapability, DecodeLedger, LedgerEntry};
pub use select::{
    auto_order, chip_label, codec_label, codec_order, decode_label, pick_audio, pick_quality,
    quality_options, select, target_quality, PlaybackSpeed, QualityOption, Selection,
};
pub use session::{Effect, Failure, FailureText, PlaybackState, Session};
