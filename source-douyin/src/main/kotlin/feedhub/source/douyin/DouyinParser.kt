package feedhub.source.douyin

import feedhub.core.DetailTarget
import feedhub.core.Emotes
import feedhub.core.FeedItem
import feedhub.core.FeedJson
import feedhub.core.ItemKind
import feedhub.core.Media
import feedhub.core.Page
import feedhub.core.ParseException
import feedhub.core.Platform
import feedhub.core.Cursor
import feedhub.core.Stats
import feedhub.core.UserRef
import feedhub.core.arr
import feedhub.core.bool
import feedhub.core.int
import feedhub.core.long
import feedhub.core.obj
import feedhub.core.string
import feedhub.core.text
import feedhub.core.toHttps
import kotlinx.serialization.json.JsonObject

/**
 * 抖音响应 → 统一模型。纯函数，单测直接喂 fixtures（见 `DouyinParserTest`）。
 *
 * ## 结构已经用真实录制确认过
 *
 * 1. **`cards[]` 的外层结构**：顶层是 `cards` + `has_more`；
 * 需要二次解析"—— 真实录制证实了这一点（`cards[].aweme` 是一段 150KB 的
 *    JSON 字符串）。[awemeOf] 仍然把几种键名 / 对象形态都试一遍：认不出来就丢掉这条卡片、不抛异常——
 *    结构不对时宁可少几条，也不要整个流报错。
 * 2. **`create_time` 是秒、`video.duration` 是毫秒**。[epochSeconds] 保留"数值大到像毫秒才除 1000"的防御。
 *
 * 这两条由 `DouyinParserTest.recordedFixtureParsesTheRealResponse` 用真实录制的 fixture 把关。
 *
 * ## 当前**不做**的东西
 *
 * 关注流、扫码登录都不做（见 [DouyinSource] 的范围）。详情 / 评论 / 播放地址现在都做了，
 * 所以卡片一律给 [DetailTarget.Native]（`id` 就是 `aweme_id`），在 App 里点开是我们的详情页。
 *
 * 表情 [FeedItem.emotes] 用 [DouyinEmoteTable] 那张表：只收 `summary`（= `desc`）里真的出现的代码
 * （`Emotes.used`），表没取到就是空表、文字照旧。
 */
internal object DouyinParser {
    private val P = DOUYIN

    /** 抖音没有标题字段，`desc` 是文案：文字代码出现在 `summary` 里。 */
    private val NO_EMOTES = emptyMap<String, String>()

    fun videoUrl(awemeId: String) = "https://www.douyin.com/video/$awemeId"
    fun noteUrl(awemeId: String) = "https://www.douyin.com/note/$awemeId"
    fun userUrl(secUid: String) = "https://www.douyin.com/user/$secUid"

    // ---------- 推荐流：/aweme/v1/web/module/feed/ ----------

    /**
     * `module/feed` 的一页。
     * 游标就是 `refresh_index`。
     */
    fun feed(json: JsonObject, refreshIndex: Int, emotes: Map<String, String> = NO_EMOTES): Page<FeedItem> {
        val cards = json.arr("cards") ?: throw ParseException(P, "推荐流缺少 cards")
        val items = cards.mapNotNull { card ->
            card.obj()?.let { awemeOf(it)?.let { a -> item(a, emotes) } }
        }
        // has_more 缺失时按"还有下一页"处理：推荐流通常没有尽头
        val hasMore = json.bool("has_more") != false
        return Page(items, if (hasMore) Cursor((refreshIndex + 1).toString()) else null)
    }

    /** 一页推荐流里所有卡片的 `desc`（判断要不要去取表情表、取回来挑哪几个用）。 */
    fun feedTexts(json: JsonObject): String =
        json.arr("cards").orEmpty().mapNotNull { card ->
            card.obj()?.let { awemeOf(it)?.text("desc") }
        }.joinToString("\n")

    /**
     * 从一张卡片里挑出 aweme 对象。`module/feed` 的 card 里 aweme 是一段 JSON 字符串，
     * 但字段名没确认，所以这里按"字符串或对象、几种常见键名、以及 card 本身就是 aweme"依次尝试。
     */
    fun awemeOf(card: JsonObject): JsonObject? {
        for (key in AWEME_KEYS) {
            val raw = card[key] ?: continue
            if (raw is JsonObject) return raw
            // 字符串形式的 aweme：解析失败就继续试别的键
            val text = card.string(key) ?: continue
            val parsed = runCatching { FeedJson.parseToJsonElement(text).obj() }.getOrNull()
            if (parsed != null) return parsed
        }
        // 有些卡片直接就把 aweme 的字段摊平在 card 自己身上
        return card.takeIf { it.text("aweme_id") != null }
    }

    /** 推荐流里的 `tab/feed` 那种平铺结构（`aweme_list`）。**没人验证过 `tab/feed` 可用**，留着当兜底。 */
    fun awemeList(json: JsonObject): List<FeedItem> =
        json.arr("aweme_list").orEmpty().mapNotNull { it.obj()?.let(::item) }

    // ---------- 一条 aweme → FeedItem ----------

    /** 一条抖音内容。广告（`is_ads`）和没有 id 的卡片丢掉。 */
    fun item(o: JsonObject, emotes: Map<String, String> = NO_EMOTES): FeedItem? {
        if (o.bool("is_ads") == true) return null
        val id = o.text("aweme_id") ?: return null
        val desc = o.text("desc")

        val images = o.arr("images").orEmpty().mapNotNull { it.obj()?.let(::image) }
        val video = o.obj("video")?.let(::videoMedia)
        val isVideo = video != null && images.isEmpty()

        val url = if (isVideo) videoUrl(id) else noteUrl(id)
        val stats = o.obj("statistics")

        return FeedItem(
            platform = P,
            id = id,
            // aweme_type：0 是视频、68 是图集，所以以媒体为准、类型号兜底
            kind = when {
                isVideo -> ItemKind.VIDEO
                images.isNotEmpty() -> ItemKind.POST
                o.int("aweme_type") == 0 -> ItemKind.VIDEO
                else -> ItemKind.POST
            },
            // 抖音没有标题字段，`desc` 是文案，所以放 summary，title 留 null（不编字段）
            title = null,
            summary = desc,
            author = author(o.obj("author")),
            publishedAt = epochSeconds(o.long("create_time")),
            media = listOfNotNull(video) + images,
            stats = Stats(
                views = stats?.long("play_count")?.takeIf { it > 0 },
                likes = stats?.long("digg_count"),
                comments = stats?.long("comment_count"),
                shares = stats?.long("share_count"),
                favorites = stats?.long("collect_count"),
            ),
            reason = null,
            // 抖音的"圈子"就是话题（`text_extra` / `cha_list`），没有就 null
            community = firstHashtag(o),
            repost = null,
            emotes = Emotes.used(emotes, desc),
            url = url,
            // 详情 / 评论 / 播放都做了，所以在 App 里原生打开；id 就是 aweme_id
            detail = DetailTarget.Native(id),
        )
    }

    private fun image(o: JsonObject): Media.Image? {
        val url = o.arr("url_list").orEmpty().firstNotNullOfOrNull { (it as? kotlinx.serialization.json.JsonPrimitive)?.content }
            ?: return null
        return Media.Image(url.toHttps(), o.int("width"), o.int("height"))
    }

    private fun videoMedia(v: JsonObject): Media.Video? {
        val cover = v.obj("cover")?.urlList() ?: v.obj("origin_cover")?.urlList()
        val durationMs = v.long("duration")
        val hasPlayAddr = v.obj("play_addr")?.arr("url_list")?.isNotEmpty() == true
        if (cover == null && durationMs == null && !hasPlayAddr) return null
        return Media.Video(
            cover = cover?.toHttps(),
            durationSec = durationMs?.takeIf { it > 0 }?.let { (it / 1000).toInt() },
            width = v.int("width"),
            height = v.int("height"),
        )
    }

    private fun JsonObject.urlList(): String? =
        arr("url_list").orEmpty().firstNotNullOfOrNull { (it as? kotlinx.serialization.json.JsonPrimitive)?.content }

    private fun author(a: JsonObject?): UserRef? {
        if (a == null) return null
        val secUid = a.text("sec_uid")
        val uid = a.text("uid")
        val id = secUid ?: uid ?: return null
        return UserRef(
            platform = P,
            id = id,
            name = a.text("nickname") ?: id,
            avatar = (a.obj("avatar_thumb")?.urlList() ?: a.obj("avatar_larger")?.urlList())?.toHttps(),
            url = secUid?.let(::userUrl),
        )
    }

    private fun firstHashtag(o: JsonObject): String? {
        o.arr("text_extra").orEmpty().forEach { e ->
            e.obj()?.text("hashtag_name")?.let { return it }
        }
        o.arr("cha_list").orEmpty().forEach { e ->
            e.obj()?.text("cha_name")?.let { return it }
        }
        return null
    }

    /**
     * `create_time` 按秒；只有数值大到像毫秒（> 10^12）才除 1000。
     * 这里不猜单位、只做量级防御。
     */
    private fun epochSeconds(value: Long?): Long? {
        if (value == null || value <= 0) return null
        return if (value > 1_000_000_000_000L) value / 1000 else value
    }

    // ---------- 登录态：/aweme/v1/web/user/profile/self/ ----------

    /** 已登录返回账号，`user` 缺失返回 null。 */
    fun selfUser(json: JsonObject): UserRef? {
        val user = json.obj("user") ?: return null
        val secUid = user.text("sec_uid")
        val uid = user.text("uid")
        val id = secUid ?: uid ?: return null
        return UserRef(
            platform = P,
            id = id,
            name = user.text("nickname") ?: id,
            avatar = user.obj("avatar_thumb")?.urlList()?.toHttps(),
            url = secUid?.let(::userUrl),
        )
    }

    /** `module/feed` 的 card 里 aweme 可能挂的键名。 */
    private val AWEME_KEYS = listOf("aweme", "aweme_info", "aweme_detail", "awemeInfo", "item")
}
