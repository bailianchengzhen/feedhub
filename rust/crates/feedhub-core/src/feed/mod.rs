//! **信息流 / 搜索 / 账号 / 关注管理的非界面逻辑**。
//!
//! 这一块原来在 `app/.../feed/`（`FeedState` / `FeedMix` / `Preload` / `ExposureTracker`）、
//! `app/.../search/SearchState.kt`、`app/.../mine/AccountsState.kt` + `AccountsCache.kt` + `FollowingState.kt`。
//! **决策、计算、状态机、缓存、落盘格式**全搬到这里，
//! Kotlin 只剩：Compose 状态容器、滚动 / 可见性测量、协程触发、真正的收发（`Source.feed`）、
//! 以及把结果写进资料库 / Cookie 罐这些宿主 IO。
//!
//! 文件：
//! - [`preload`]：预加载策略（按屏触发 / 图片预取范围 / 内存缓存上限）+ `FeedPreloadTrigger` 状态机；
//! - [`exposure`]：可见比例几何、曝光门槛与停留计时状态机（宿主喂"这一刻可见了哪些、比例多少"）；
//! - [`mix`]：`FeedMix.kt` 里还在 Kotlin 的那部分（比例配额 / 个性化过滤原型 `mixBatch`）；
//! - [`feeds`]：信息流的**状态机本体**（每个平台的游标、去重、批 id、平台序号、预取、晚到 / 超时、
//!   刷新 / 翻页 / 重试 / 出错策略）——宿主只做协程和收发；
//! - [`accounts`]：账号状态的判定 + 本地缓存（`app.accounts`）的编解码；
//! - [`following`]：关注管理的分页 + 取关确认流程（**写操作**，流程原样）。
//!
//! 三条纪律（和 `brain` 一致）：
//! 1. **不读时钟**：`now_ms` 一律由宿主传（测试用假时钟 / 虚拟时间）；
//! 2. **不取随机数**：批种子由宿主给，同一个种子两次调用结果一样；
//! 3. **不碰 IO**：收发 / 资料库 / Cookie 罐都在宿主（这里只用宿主传进来的元数据做决定）。

pub mod accounts;
pub mod exposure;
pub mod feeds;
pub mod following;
pub mod mix;
pub mod preload;
pub mod search;

