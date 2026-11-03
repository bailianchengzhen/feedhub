package feedhub.source.xiaohongshu

import feedhub.core.Cursor
import feedhub.core.FeedItem
import feedhub.core.Page
import feedhub.rust.RustXhsHosted

/**
 * **关注流"取一页"的接缝**：调度 / 三个接口的请求 / 归并 / 存盘都在 Rust 平台对象里，
 * Kotlin 只剩"我自己是谁"和"游标原样传"。
 *
 * 两个实现：
 * - `HostedXhsFollowApi`（`XhsHosted.kt`）：走 Rust 平台对象，**生产用这个**；
 * - 参照实现：以前那一套（[XhsFollowFeed] + [KotlinXhsFeedApi]），`XhsSource` 里在
 *   `parse` 不是 `XhsHostedParse` 时用它——那是**对拍**的对照，也是搬之前的唯一路径。
 *
 * 为什么 `selfId` 不在这个接口上：Rust 在**发关注列表请求之前**才需要它，而"没有存过 user_id 就
 * 先问一次 `user/me`"那一步要发请求（也是签名 / 限速的一部分），位置必须和 Kotlin 参照实现一模一样
 * （一次刷新最多一次）。所以它做成 Rust → Kotlin 的**回调**（`HostXhsSelfId`），不在调用参数里。
 */
internal interface XhsFollowRustApi {
    /** 一页 + 这次刷新有没有真的联网（`false` = 纯缓存翻页）。 */
    suspend fun page(cursor: Cursor?): Pair<Page<FeedItem>, Boolean>

    /** 手上那份缓存（排障 / 测试用；不发请求）。 */
    suspend fun cached(): List<RustXhsHosted.FollowAuthor>

    /** 上次问 `followings/extra` 的时间（没读过就是 null）。 */
    suspend fun extraAt(): Long?
}
