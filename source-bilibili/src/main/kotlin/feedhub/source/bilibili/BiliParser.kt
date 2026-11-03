package feedhub.source.bilibili

import feedhub.core.Cursor
import feedhub.core.DetailTarget
import feedhub.core.Emotes
import feedhub.core.FeedItem
import feedhub.core.FeedJson
import feedhub.core.ItemKind
import feedhub.core.Media
import feedhub.core.Page
import feedhub.core.ParseException
import feedhub.core.Platform
import feedhub.core.Stats
import feedhub.core.UserRef
import feedhub.core.arr
import feedhub.core.bool
import feedhub.core.int
import feedhub.core.long
import feedhub.core.obj
import feedhub.core.parseCount
import feedhub.core.parseDuration
import feedhub.core.path
import feedhub.core.string
import feedhub.core.text
import feedhub.core.toHttps
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive

/** B站响应 → 统一模型。纯函数，单测直接喂 fixtures。 */
internal object BiliParser {
    private val P = BILIBILI

    /** 有原生详情的动态类型（新版图文 / 文字也是 DRAW，专栏是 ARTICLE）。 */
    private val NATIVE_DYNAMIC_TYPES = setOf("DYNAMIC_TYPE_DRAW", "DYNAMIC_TYPE_WORD", "DYNAMIC_TYPE_FORWARD", "DYNAMIC_TYPE_ARTICLE")

    fun videoUrl(bvid: String) = "https://www.bilibili.com/video/$bvid"
    fun dynamicUrl(id: String) = "https://t.bilibili.com/$id"
    fun spaceUrl(mid: String) = "https://space.bilibili.com/$mid"

    // ---------- 推荐流：/x/web-interface/wbi/index/top/feed/rcmd ----------

    /** [freshIdx] 是这一页的序号；推荐流没有尽头，下一页永远是 freshIdx + 1。 */
    fun recommend(json: JsonObject, freshIdx: Int): Page<FeedItem> {
        val items = json.path("data")?.arr("item") ?: throw ParseException(P, "推荐流缺少 data.item")
        return Page(items.mapNotNull { it.obj()?.let(::recommendItem) }, Cursor((freshIdx + 1).toString()))
    }

    /** 只要普通视频（goto=av）；直播、广告、番剧卡片跳过。 */
    fun recommendItem(o: JsonObject): FeedItem? {
        if (o.string("goto") != "av") return null
        return BiliVideoParser.videoCard(o)
    }

    // ---------- 动态：/x/polymer/web-dynamic/v1/feed/all 和 feed/space ----------

    fun dynamics(json: JsonObject): Page<FeedItem> {
        val data = json.obj("data") ?: throw ParseException(P, "动态流缺少 data")
        val items = data.arr("items") ?: throw ParseException(P, "动态流缺少 data.items")
        val offset = data.text("offset")
        val hasMore = data.bool("has_more") == true
        return Page(items.mapNotNull { it.obj()?.let(::dynamic) }, if (hasMore && offset != null) Cursor(offset) else null)
    }

    /**
     * 一条动态。字段结构见 bilby `data/DynamicCardMapper.kt`。
     * 视频类动态的 id 用 bvid，这样和推荐流里同一个视频的 [FeedItem.key] 一致、能去重。
     */
    fun dynamic(o: JsonObject): FeedItem? {
        val idStr = o.text("id_str")
        val type = o.string("type")
        val modules = o.obj("modules") ?: return null
        val author = modules.obj("module_author")
        val dyn = modules.obj("module_dynamic")
        val stat = modules.obj("module_stat")
        val major = dyn?.obj("major")
        val desc = dyn?.obj("desc")?.text("text")

        val base = FeedItem(
            platform = P,
            id = idStr ?: return deletedOrig(major),
            kind = ItemKind.MOMENT,
            summary = desc,
            author = author?.let { user(it.string("mid"), it.text("name"), it.text("face")) },
            publishedAt = author?.long("pub_ts")?.takeIf { it > 0 },
            stats = Stats(
                likes = stat?.obj("like")?.long("count"),
                comments = stat?.obj("comment")?.long("count"),
                shares = stat?.obj("forward")?.long("count"),
            ),
            reason = author?.text("pub_action"),
            community = dyn?.obj("topic")?.text("name"),
            url = dynamicUrl(idStr),
            // 图文 / 文字 / 转发 / 专栏有原生详情（id 带 dyn: 前缀，见 BiliIds）；视频在 archive() 里改成 bvid，直播、番剧等继续 Web
            detail = if (type in NATIVE_DYNAMIC_TYPES) DetailTarget.Native(BiliIds.dynamic(idStr)) else DetailTarget.Web(dynamicUrl(idStr)),
        )

        val withMajor = when (major?.string("type")) {
            "MAJOR_TYPE_ARCHIVE" -> major.obj("archive")?.let { archive(base, it) }
            "MAJOR_TYPE_UGC_SEASON" -> major.obj("ugc_season")?.let { archive(base, it) }
            "MAJOR_TYPE_OPUS" -> major.obj("opus")?.let { opus(base, it, isArticle = type == "DYNAMIC_TYPE_ARTICLE") }
            "MAJOR_TYPE_DRAW" -> major.obj("draw")?.let { draw ->
                base.copy(media = draw.arr("items").orEmpty().mapNotNull { it.obj()?.let(::drawImage) })
            }
            "MAJOR_TYPE_ARTICLE" -> major.obj("article")?.let { a ->
                val url = a.text("jump_url")?.toHttps() ?: base.url!!
                base.copy(
                    kind = ItemKind.ARTICLE, title = a.text("title"), summary = a.text("desc") ?: desc,
                    media = a.arr("covers").orEmpty().mapNotNull { (it as? JsonPrimitive)?.content?.let { u -> Media.Image(u.toHttps()) } },
                    url = url,
                )
            }
            "MAJOR_TYPE_PGC" -> major.obj("pgc")?.let { pgc ->
                val url = pgc.text("jump_url")?.toHttps() ?: base.url!!
                val s = pgc.obj("stat")
                base.copy(
                    kind = ItemKind.VIDEO, title = pgc.text("title"),
                    media = listOf(Media.Video(cover = pgc.text("cover")?.toHttps())),
                    stats = base.stats.copy(views = parseCount(s?.string("play")), danmaku = parseCount(s?.string("danmaku"))),
                    url = url, detail = DetailTarget.Web(url),
                )
            }
            "MAJOR_TYPE_LIVE_RCMD" -> major.obj("live_rcmd")?.let { live ->
                // content 是一段 JSON 字符串
                val info = live.text("content")?.let { runCatching { FeedJson.parseToJsonElement(it).obj() }.getOrNull() }
                    ?.obj("live_play_info")
                live(base, info?.text("title"), info?.text("cover"), info?.text("link"))
            }
            "MAJOR_TYPE_LIVE" -> major.obj("live")?.let { live(base, it.text("title"), it.text("cover"), it.text("jump_url")) }
            "MAJOR_TYPE_COMMON" -> major.obj("common")?.let { c ->
                base.copy(
                    title = c.text("title"), summary = listOfNotNull(desc, c.text("desc")).joinToString("\n").ifEmpty { null },
                    media = listOfNotNull(c.text("cover")?.let { Media.Image(it.toHttps()) }),
                )
            }
            else -> null
        } ?: base

        val repost = if (type == "DYNAMIC_TYPE_FORWARD") o.obj("orig")?.let(::dynamic) else null
        // 摘要是纯文本，里面的 [打call] 对应的图片在 rich_text_nodes 里（动态文字在 desc，图文在 opus.summary）；
        // 转发的原动态有自己的 emotes
        val table = BiliDynamicParser.emoteTable(dyn?.obj("desc")?.arr("rich_text_nodes")) +
            BiliDynamicParser.emoteTable(major?.obj("opus")?.obj("summary")?.arr("rich_text_nodes"))
        return withMajor.copy(repost = repost, emotes = Emotes.used(table, withMajor.title, withMajor.summary))
    }

    /** 被转发的原动态被删了：没有 id_str，major 是 MAJOR_TYPE_NONE。 */
    private fun deletedOrig(major: JsonObject?): FeedItem? {
        val tips = major?.obj("none")?.text("tips") ?: return null
        return FeedItem(platform = P, id = "deleted", kind = ItemKind.OTHER, summary = tips, detail = DetailTarget.Web("https://t.bilibili.com/"))
    }

    private fun archive(base: FeedItem, a: JsonObject): FeedItem {
        val bvid = a.text("bvid") ?: return base.copy(kind = ItemKind.VIDEO, title = a.text("title"))
        val s = a.obj("stat")
        val url = videoUrl(bvid)
        return base.copy(
            id = bvid,
            kind = ItemKind.VIDEO,
            title = a.text("title"),
            // 视频简介比"投稿了视频"那句动态文字有用；动态文字为空时才用它
            summary = base.summary ?: a.text("desc")?.takeIf { it != "-" },
            media = listOf(Media.Video(cover = a.text("cover")?.toHttps(), durationSec = parseDuration(a.text("duration_text")))),
            stats = base.stats.copy(views = parseCount(s?.string("play")), danmaku = parseCount(s?.string("danmaku"))),
            url = url,
            detail = DetailTarget.Native(bvid),
        )
    }

    private fun opus(base: FeedItem, op: JsonObject, isArticle: Boolean): FeedItem {
        val url = op.text("jump_url")?.toHttps() ?: base.url!!
        return base.copy(
            kind = if (isArticle) ItemKind.ARTICLE else ItemKind.MOMENT,
            title = op.text("title"),
            summary = op.obj("summary")?.text("text") ?: base.summary,
            media = op.arr("pics").orEmpty().mapNotNull { it.obj()?.let(::opusImage) },
            url = url,
        )
    }

    private fun live(base: FeedItem, title: String?, cover: String?, link: String?): FeedItem {
        val url = link?.toHttps() ?: base.url!!
        return base.copy(
            kind = ItemKind.LIVE, title = title,
            media = listOfNotNull(cover?.let { Media.Image(it.toHttps()) }),
            url = url, detail = DetailTarget.Web(url),
        )
    }

    private fun opusImage(p: JsonObject): Media.Image? =
        p.text("url")?.let { Media.Image(it.toHttps(), p.int("width"), p.int("height")) }

    private fun drawImage(p: JsonObject): Media.Image? =
        p.text("src")?.let { Media.Image(it.toHttps(), p.int("width"), p.int("height")) }

    // ---------- 账号：/x/web-interface/nav ----------

    /** 已登录返回账号，未登录返回 null。 */
    fun nav(json: JsonObject): UserRef? {
        val data = json.obj("data") ?: return null
        if (data.bool("isLogin") != true) return null
        return user(data.string("mid"), data.text("uname"), data.text("face"))
    }

    fun wbiKeys(json: JsonObject): Pair<String, String>? {
        val img = json.path("data", "wbi_img") ?: return null
        val imgUrl = img.text("img_url") ?: return null
        val subUrl = img.text("sub_url") ?: return null
        return WbiSigner.keyFromUrl(imgUrl) to WbiSigner.keyFromUrl(subUrl)
    }

    fun user(mid: String?, name: String?, face: String?): UserRef? {
        if (mid.isNullOrEmpty() || mid == "0") return null
        return UserRef(P, mid, name ?: mid, face?.toHttps(), spaceUrl(mid))
    }
}
