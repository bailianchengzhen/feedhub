package feedhub.source.xiaohongshu

import feedhub.core.ApiException
import feedhub.core.PlayInfo
import feedhub.core.Platform
import feedhub.core.RequestPriority
import feedhub.core.obj
import kotlinx.serialization.json.JsonObject

/**
 * **播放地址"取一次"的接缝**。
 *
 * 两个实现：
 * - [KotlinXhsPlayApi]：就是搬进 Rust 之前 `XhsSource.play` 那几行（`recentNotes` 缓存 + `POST feed` +
 *   [XhsVideo.play]），行为和搬之前**一模一样**，是对拍的参照；
 * - `HostedXhsPlayApi`（`XhsHosted.kt`）：Rust 平台对象拼请求 / 签名 / 判错，`XhsVideo` 的解析也在 Rust
 *   （`feedhub-core/src/xhs/play.rs`），**"最近取过的笔记"那份缓存也在 Rust 平台对象里**——
 *   点开视频再播放时一个请求都不发。
 *
 * 两家都遵守同一条口径（[XhsSource.play] 的注释里写着为什么）：
 * `partId` 不是 [XhsVideo.MAIN] 时**一个请求都不发**、直接报错；小红书没有按清晰度挑流的参数，
 * [quality] 收下但一个字节都不影响。
 */
internal interface XhsPlayApi {
    /** 播放地址。[nowMs] 是这一次的"现在"（签名 / 缓存时限都用它）。 */
    suspend fun play(id: String, partId: String, quality: Int?, nowMs: Long): PlayInfo

    /**
     * Rust 那边**手上有没有**这条笔记刚取过的详情（5 分钟内、最多 8 条）。
     *
     * 只给 `HostedXhsPlayApi` 用：命中时不发请求也就不签名，**不能从 `Random` 里抽随机量**
     * （签名的取用顺序要和 Kotlin 参照实现逐位对齐，见 [XhsSignRandomDrawer]）。参照实现永远是 `false`。
     */
    fun cached(id: String, nowMs: Long): Boolean = false
}

/**
 * Kotlin 参照实现：搬进 Rust 之前的 `XhsSource.play` + `fetchNote` + `recentNote`，语义一行没改。
 *
 * 缓存是 `XhsSource` 里那个 [recentNotes]（访问序 LRU、8 条、5 分钟）：`detail` 取过的笔记也往里写，
 * 所以"点开视频 → 马上播放"只有一个 `POST feed`。
 */
internal class KotlinXhsPlayApi(
    private val api: XhsApi,
    private val platform: Platform,
    private val now: () -> Long,
    /** 手上那条笔记的详情（没取过 / 超过 [NOTE_REUSE_MS] 就是 null）。 */
    private val recentNote: (String) -> JsonObject?,
    /** 取到详情之后记住它（`XhsSource.recentNotes`）。 */
    private val remember: (String, JsonObject) -> Unit,
) : XhsPlayApi {
    override suspend fun play(id: String, partId: String, quality: Int?, nowMs: Long): PlayInfo {
        if (partId != XhsVideo.MAIN) throw ApiException(platform, "part", "小红书的视频只有一个分段 main，没有 $partId")
        val ref = XhsParser.parseNativeId(id)
        // 刚打开详情时取过这条笔记就直接用，不再发一次详情请求
        val note = recentNote(ref.noteId) ?: fetchNote(ref)
        return XhsVideo.play(XhsNote.noteCard(note))
    }

    /** `POST feed`（用户正等着开播：[RequestPriority.INTERACTIVE]），取到就记住。 */
    private suspend fun fetchNote(ref: XhsParser.NoteRef): JsonObject =
        api.post(
            FEED,
            feedBody(ref.noteId, ref.xsecSource ?: SOURCE_FEED, ref.xsecToken.orEmpty()),
            priority = RequestPriority.INTERACTIVE,
        ).also { json -> remember(ref.noteId, json) }
}
