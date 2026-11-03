package feedhub.source.bilibili

import feedhub.core.Cursor
import feedhub.core.DetailTarget
import feedhub.core.FeedItem
import feedhub.core.ItemKind
import feedhub.core.Media
import feedhub.core.Page
import feedhub.core.ParseException
import feedhub.core.SearchKind
import feedhub.core.SearchRequest
import feedhub.core.SearchSort
import feedhub.core.Stats
import feedhub.core.UserRef
import feedhub.core.arr
import feedhub.core.int
import feedhub.core.long
import feedhub.core.obj
import feedhub.core.parseDuration
import feedhub.core.string
import feedhub.core.stripHtml
import feedhub.core.text
import feedhub.core.toHttps
import kotlinx.serialization.json.JsonObject

/**
 * B站搜索（`/x/web-interface/wbi/search/type`，`search_type=video`）→ 统一模型。纯函数，单测喂夹具。
 *
 * 响应形状（参考 BiliPai `SearchModels.kt` 的 `SearchTypeData` / `SearchVideo`）：
 * `{code, data:{page, pagesize, numResults, numPages, result:[…]}}`。
 * 条目字段：`bvid` / `title`（带 `<em class="keyword">` 高亮标签）/ `pic`（`//i0.hdslb.com/…`）/
 * `author` / `mid` / `upic` / `pubdate`（Unix 秒）/ `duration`（`"12:34"`）/ `play` / `video_review` /
 * `review` / `favorites` / `like` / `arcurl` / `description`（也带 `<em>`）。
 *
 * 所以 `ALL` 和 `VIDEO` 都走 `search_type=video`。
 */
internal object BiliSearchParser {
    private val P = BILIBILI

    /** `search/type` 的一页。游标是页码（从 1 起）。 */
    fun page(json: JsonObject, page: Int): Page<FeedItem> {
        val data = json.obj("data") ?: throw ParseException(P, "搜索结果缺少 data")
        val results = data.arr("result").orEmpty()
        val items = results.mapNotNull { it.obj()?.let(::video) }
        val numPages = data.int("numPages") ?: 0
        val hasMore = items.isNotEmpty() && numPages > page
        return Page(items, if (hasMore) Cursor((page + 1).toString()) else null)
    }

    /** 一条视频搜索结果。`title` / `description` 里的高亮标签去掉；`pic` 补上 `https:`。 */
    fun video(o: JsonObject): FeedItem? {
        val bvid = o.text("bvid") ?: return null
        val title = o.text("title")?.let(::clean)?.takeIf { it.isNotBlank() }
        val description = o.text("description")?.let(::clean)?.takeIf { it.isNotBlank() }
        val author = o.text("author")?.takeIf { it.isNotBlank() }
        val mid = o.text("mid")
        val duration = parseDuration(o.text("duration"))
        return FeedItem(
            platform = P,
            id = bvid,
            kind = ItemKind.VIDEO,
            title = title,
            summary = description,
            author = author?.let { UserRef(platform = P, id = mid.orEmpty(), name = it, avatar = o.text("upic")?.toHttps()) },
            publishedAt = o.long("pubdate")?.takeIf { it > 0 },
            media = listOf(Media.Video(cover = o.text("pic")?.toHttps(), durationSec = duration)),
            stats = Stats(
                views = o.long("play"),
                // video_review 是弹幕数（BiliPai 的字段名）；review 是评论数
                danmaku = o.long("video_review"),
                comments = o.long("review"),
                favorites = o.long("favorites"),
                likes = o.long("like"),
            ),
            // 统一用 bvid 的规范地址（真实响应里的 arcurl 是 av 号形态，分享出去也对，但和信息流里的同一条不一致）
            url = BiliParser.videoUrl(bvid),
            detail = DetailTarget.Native(bvid),
        )
    }

    /** 去掉 `<em class="keyword">` 之类的高亮标签、还原实体（参考实现的 `cleanSearchText`）。 */
    internal fun clean(html: String): String = stripHtml(html).replace(Regex("[ \\t\\u3000]{2,}"), " ").trim()

    /** 排序档 → `order` 参数（`SearchRepository.kt:703-711`）。 */
    fun order(sort: SearchSort): String = when (sort) {
        SearchSort.RELEVANCE -> "totalrank"
        SearchSort.LATEST -> "pubdate"
        SearchSort.HOT -> "click"
    }

    /** 搜索类型 → `search_type`。 */
    fun searchType(kind: SearchKind): String = when (kind) {
        SearchKind.ALL, SearchKind.VIDEO, SearchKind.POST -> "video"
        SearchKind.USER, SearchKind.TOPIC -> throw ParseException(P, "暂不支持搜 ${kind.name}（FeedItem 没有对应模型）")
    }

    /** 请求参数（照 BiliPai `SearchRepository` 的公共参数 + 视频那一组）。 */
    fun params(request: SearchRequest, page: Int, pageSize: Int): List<Pair<String, String>> = listOf(
        "keyword" to request.text,
        "search_type" to searchType(request.kind),
        "order" to order(request.sort),
        "page" to page.toString(),
        "page_size" to pageSize.toString(),
        "platform" to "pc",
        "web_location" to "1430654",
    )
}
