//! feedhub 的纯 Rust 核心。
//!
//! 边界：**平台层（请求怎么发 + 解析）在 Rust，
//! 真正的收发留在宿主**。网络 / TLS / Cookie / 代理 / 限速一直在 Kotlin（OkHttp），
//! 平台的连接特征一个字节都不变。
//!
//! 所以这一层分两半：
//! - **纯函数**：模型（[`model`]）、JSON 容错取值（[`json`]）、各平台的解析与签名（`bili` / `zhihu` / `tieba` / `heybox` / `coolapk` / `xhs` / `douyin` / `reddit` / `tiktok` / `x` / `youtube`）；
//! - **平台对象**：`<平台>::source::XxxHostedSource` 负责"拼 URL / 参数 / 头、翻页游标、签名的 key 与缓存、
//!   错误判定"，需要宿主的三件事通过 [`host`] 里的三个 trait 反向回调（HTTP / 键值存储 / Cookie 罐）。
//! - 几家共用的零件（query 编码、OkHttp 的 query 规范化、Java 的空白口径、JSON 字符串、MD5、头查找、
//!   日期换算）在 [`hosted`]，**口径不同的**（TikTok 的 RFC 3986、抖音签名的规范化、YouTube 的大写 `\u`）
//!   留在各自模块，`hosted` 的文件头写着为什么不合。

// 桌面对外接口的协议常量 + 取图决策。unix socket 与 Coil 取图仍在宿主。
pub mod appapi;
pub mod account;
pub mod bili;
pub mod brain;
pub mod coolapk;
// **Cookie 罐**：`core/Cookies.kt` + `core/CookieStore.kt` 的 Rust 版。
// 匹配 / 过期 / 去重 / 读改写合并 / `cookies.json` 编解码 / 留底 / WebView Cookie 串全在这里；
// 宿主只剩 OkHttp 的薄适配和"文本放哪儿"。见 cookies.rs 的文件头。
pub mod cookies;
// **从 Chrome 导入 Cookie**：`ChromeCookieImporter` 的决策 / 解密 / 分组那一半。
// 打开 SQLite、复制库文件、跑 `kwallet-query` 留在宿主。见 chrome_cookies.rs 的文件头。
pub mod chrome_cookies;
// 详情 / 评论的通用模型。信息流那半边在 `model`，这一层只装 `NativeDetail.kt` 那一套。
pub mod detail;
pub mod douyin;
// 本地向量的分词器：`app/.../embed/GemmaTokenizer.kt` 的 Rust 版。
pub mod embed;
// **信息流 / 搜索 / 账号 / 关注管理的非界面逻辑**：
// 预加载策略、曝光判定与计时、信息流状态机（游标 / 去重 / 批 id / 预取 / 晚到）、账号状态 + 本地缓存格式、
// 关注管理 + 取关流程。见 `feed/mod.rs`。
pub mod feed;
pub mod heybox;
pub mod host;
pub mod hosted;
// HTML → 树 / 富文本：
// `html::dom` 是 HTML5 分词 + "in body" 树构造（jsoup 的替身，加固版），`html` 本身是
// `core/HtmlRichText.kt` 的移植 + 字符引用解码 + Kotlin/Java 空白口径。知乎和小黑盒的正文都走它。
pub mod html;
pub mod json;
// 本地资料库：Rust + rusqlite，同一个库文件 / 同一套表结构。见 library/mod.rs。
pub mod library;
pub mod model;
// 播放 / 弹幕的通用模型（Kotlin `PlayInfo` / `Danmaku`）：B站 是第一个搬进来的平台，
// 后面的抖音 / TikTok / 其余几家只写"响应怎么变成本模型"，别再造一套 record。见 play.rs。
// 展示层的纯函数：`core/Text.kt` / `core/TimeText.kt` /
// `app/ui/Format.kt` / `app/detail/DetailFormat.kt` / `app/mine/DayLabel.kt` /
// `app/feed/FeedEmpty.kt` / `app/video/{VideoPosts,ImmersionFollow,MediaPlan}.kt` /
// `app/detail/FailureText.kt` 里"不是画界面"的那一半。时区 / 时钟由宿主传进来。见 present/mod.rs。
pub mod present;
pub mod play;
// 播放器的决策层：选流 / 解码账本 / 弹幕 / 清单改写 / 播放状态机。
// Kotlin 的 `app/.../video/` 里"不是界面、也不是引擎调用"的那一半全在这里，宿主只剩
// GStreamer / Media3 的调用、中转的 socket、每帧的绘制。见 player/mod.rs。
pub mod player;
// 按平台指定代理的判定：`resolveProxyMode` 那一套的 Rust 版，App / cli 都走它。
pub mod proxy;
// 内置代理核心：`core/proxy/` 那一包的 Rust 版——订阅 / 分享链接 / Clash YAML
// 的解析、sing-box 配置生成、`proxy-core.json` 的逐字节读写、sha256、核心子进程的状态机、
// 「只在前台跑」的判定表。**`proxy` 是"按平台怎么走代理"，`proxy_core` 是"内置那个 core"**，别混。
pub mod proxy_core;
// **限速 / 冷却**：`core/RateLimit.kt` 的排队决策 + cli 的跨进程限速 / 冷却文件。
// 只算"该等多久 / 谁先发"，睡和锁在宿主。见 ratelimit.rs 的文件头。
pub mod ratelimit;
pub mod reddit;
// 设置的读 / 迁移 / 归一化 / 默认值：
// 存盘的字节仍由 kotlinx 写，"该是什么值"只有 Rust 一份。
pub mod settings;
pub mod sync;
pub mod thumb;
pub mod tieba;
// 本地翻译的纯逻辑：语言识别 / 缓存 / 排队 / llama-server 进程与
// Vulkan 设备 / 富文本拼回。见 translate/mod.rs 的文件头。
pub mod translate;
pub mod tiktok;
pub mod x;
pub mod xhs;
pub mod youtube;
pub mod zhihu;

/// 手写的极简 protobuf 读取器（最初随贴吧的 `pb/page` / `pb/floor` 进来，实现仍在 `tieba/pb.rs`）。
/// 所以在这里再导出一份：
/// `crate::pb::walk(...)`。下级平台要读 protobuf 就直接用，**别再写第三个 reader**。
pub use tieba::pb;

/// 这个 `.so` 的版本（FFI 里会给 Kotlin 看一眼，确认加载的是同一份构建）。
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

// **扫码登录的共用件**：`core/QrLoginSupport.kt` 的 `QrSessions` LRU + 二维码 JSON 解析。
// Cookie 罐（宿主对象）仍在 Kotlin，这里只有"谁该被挤掉"和"响应是不是 JSON 对象"。见 qr/mod.rs。
pub mod qr;
// **插件描述的自检**：`core/plugin/PluginContract.checkDescriptor` 的 Rust 版
// （id / 颜色 / 域名 / 能力 / 端 / 登录域 / 表情表 / 搜索能力的一致性与文案）。见 plugin_contract.rs。
pub mod plugin_contract;
pub mod captcha;

// **性能记录**：proc 解析 / 帧统计分位数 / 按天文件轮转与淘汰 / JSON 组装与状态机。见 perf/mod.rs。
pub mod perf;
