//! **本地翻译的纯逻辑**。
//!
//! 搬进来的是 `app/.../translate/` 那一套的**决策**部分，留在宿主的是**界面**和**真正把 HTTP 发出去**这两类：
//!
//! | 这里 | 宿主（Kotlin） | 为什么 |
//! |---|---|---|
//! | [`lang`] 语言识别（lingua）、门控 `shouldAutoTranslate` | — | 纯判定 |
//! | [`cache`] `translations.jsonl` 的键 / 格式 / 去重 / 落盘 | 一个 IO 线程（别占 Main） | 纯格式 + 文件 |
//! | [`queue`] 排队：新的优先 / 并发上限 / 去重 / 取消 | `CompletableDeferred`（协程挂起） | 状态机在这边，等待在宿主 |
//! | [`text`] 哪些块能翻、译文怎么拼回富文本 | Compose 渲染 | 纯变换 |
//! | [`server`] `--list-devices` 解析 / 按名字挑 AMD 核显 / 重启策略 | — | 纯规则 + `std::process` |
//! | [`device`] 手机上的后端候选（NPU → GPU → NPU+GPU → CPU）/ 降级并记住 | — | 纯规则 + `ctx.store` |
//! | [`engine`] llama-server 的启停 / 就绪轮询 / 一次 chat 请求 | 发请求（[`crate::host::HttpHost`]）、60 秒的心跳定时器 | `.so` 里没有网络栈、也不该有定时器 |
//!
//! 三条口径和其余层一致：
//! - **时间由宿主注入**（`now_ms`）：空闲回收、重启判断都要能对拍；
//! - **随机 / 语言模型不掺假**：识别用的是同一份 lingua 模型，对拍比对"中文 / 外语"这个二值判定；
//! - **`.so` 里没有 tokio / 网络栈**：到本机 `llama-server` 的 HTTP 走 [`crate::host::HttpHost`]。
//!
//! 和平台层不一样的一点：这里的 HTTP **不是**发给平台的，而是发给本机回环上的 llama-server
//! （宿主那个客户端是 `Proxy.NO_PROXY` 的专用客户端）。所以 [`crate::host::HostError`] 的
//! `ProxyUnavailable` 这一档在这里不会出现，失败一律当普通网络错误。

pub mod cache;
pub mod device;
pub mod engine;
// 界面入口的判定与状态：出不出「翻译」入口 / 现在长什么样 /
// 显示原文还是译文 / 灰字里的语言名。**只有判断**，Compose 那一半在宿主。
pub mod entry;
pub mod lang;
pub mod queue;
pub mod server;
pub mod text;

/// 本地翻译服务的状态（Kotlin `TranslationStatus`，四档一字不差）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// 还没启动（没开翻译，或者还没有人请求过）。
    Stopped,
    /// `llama-server` 正在加载模型 / 第一次请求正在等它。
    Starting,
    /// 模型已经加载，可以翻译。
    Ready,
    /// 启动失败 / 模型路径不对 / 进程挂了。
    Failed,
}

impl Status {
    /// 给日志 / 错误文案用的名字（和 Kotlin 枚举名一样）。
    pub fn name(self) -> &'static str {
        match self {
            Status::Stopped => "STOPPED",
            Status::Starting => "STARTING",
            Status::Ready => "READY",
            Status::Failed => "FAILED",
        }
    }
}

/// **状态变化的出口**：Rust 决定状态，宿主把它显示出来（设置页 / 转圈）。
///
/// 为什么是回调而不是让宿主猜：`startServer` 从 `STOPPED` 走到 `STARTING` 再到 `READY` 中间要
/// 等模型加载（最长 120 秒），宿主要在这期间就把"正在加载"显示出来。宿主实现里只做一件事：
/// 写进 `MutableStateFlow<TranslationStatus>`（线程安全，可能在 IO 线程上被调）。
pub trait StatusHost: Send + Sync {
    fn status(&self, status: Status);
}

/// 什么都不做的状态出口（测试 / 不关心状态的调用方用）。
pub struct NoStatus;

impl StatusHost for NoStatus {
    fn status(&self, _status: Status) {}
}

/// **语言识别器**（"这段是哪种语言"）。
///
/// 生产路径用 [`lang::Lingua`]（Rust 版 lingua，只带中 / 英两张模型）。
/// 这个 trait 存在的理由有两个：
/// 1. 对拍要能数"识别被调了几次"（验证「没点亮的平台在语言识别**之前**就返回了」那条产品规则）；
/// 2. `Identifier` 在 Kotlin 侧本来就是个接口（测试用假识别器），搬过来以后要把那条缝留着。
pub trait Detector: Send + Sync {
    /// BCP 47 标签（`zh` / `en`）；识别不出来是 `None`（= Kotlin 那边 `null`，上层按外语处理）。
    fn detect(&self, text: &str) -> Option<String>;
}

/// **翻译门控的输入**（都是本机设置，每次调用现传；Rust 不存设置）。
///
/// 字段和 Kotlin `AppSettings` 的对应关系：`enabled` = `translationOn`、`target` = `translationTarget`、
/// `auto_platforms` = `translationPlatforms`（平台 id 字符串，顺序无关，只做"在不在里面"）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Gate {
    pub enabled: bool,
    pub target: String,
    pub auto_platforms: Vec<String>,
    /// 当前引擎支持的源语言标签（如 `["en"]`）；**空列表 = 不限制源语言**（凡不是 `target` 的外语都翻）。
    pub source_languages: Vec<String>,
}

impl Gate {
    /// 这个平台要不要自动翻（`platform` 为 `None` = 不在详情里，不按平台限制）。
    pub fn allows_platform(&self, platform: Option<&str>) -> bool {
        match platform {
            None => true,
            Some(p) => self.auto_platforms.iter().any(|it| it == p),
        }
    }

    /// [`tag`] 是不是这次该翻的源语言：既不能是目标语言，且在限制了 `source_languages` 时必须落在支持列表里。
    pub fn allows_source(&self, tag: &str) -> bool {
        !lang::matches_target(tag, &self.target)
            && (self.source_languages.is_empty()
                || lang::matches_any_lang(tag, &self.source_languages))
    }
}
