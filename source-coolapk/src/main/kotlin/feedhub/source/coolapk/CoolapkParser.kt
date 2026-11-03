package feedhub.source.coolapk

import feedhub.core.Block
import feedhub.core.Comment
import feedhub.core.Cursor
import feedhub.core.DetailTarget
import feedhub.core.FeedDetail
import feedhub.core.FeedItem
import feedhub.core.FeedJson
import feedhub.core.ItemKind
import feedhub.core.Media
import feedhub.core.Page
import feedhub.core.ParseException
import feedhub.core.Platform
import feedhub.core.RichText
import feedhub.core.Span
import feedhub.core.Stats
import feedhub.core.UserRef
import feedhub.core.arr
import feedhub.core.long
import feedhub.core.obj
import feedhub.core.string
import feedhub.core.text
import feedhub.core.toHttps
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.contentOrNull

/**
 * 酷安响应 → 统一模型。纯函数，单测直接喂 fixtures。
 *
 * 列表接口（indexV8、dataList、replyList）都是 `{"data": [实体...]}`，实体用 `entityType` 区分：
 * `feed` 是动态 / 图文 / 问答等内容，`feed_reply` 是评论，`card` 是各种卡片（轮播图、金刚区图标、话题横滑、
 * "优质图文"横滑、评论区里的推广卡 feedDetailReplySponsorCard……）。c001apk-flutter 的
 * `constants.dart` 里 `entityTemplateList` 列的就是这些卡片。统一信息流只要内容，所以只留 `feed`（评论区只留
 * `feed_reply`），卡片一律丢掉。
 */
internal object CoolapkParser {
    private val P = COOLAPK

    fun feedUrl(id: String) = "${CoolapkHtml.WEB}/feed/$id"

    // ---------- 列表 ----------

    /** 推荐流（indexV8）和关注流（dataList）。 */
    fun feedList(json: JsonObject, cursor: FeedCursor): Page<FeedItem> {
        val data = json.arr("data") ?: throw ParseException(P, "列表缺少 data 数组")
        val entities = data.mapNotNull { it as? JsonObject }
        val items = entities.filter { it.string("entityType") == "feed" }.mapNotNull(::feed)
        return Page(items, nextCursor(entities, "feed", cursor))
    }

    /** 关注流在没登录 / 登录失效时返回的页面：第一张卡是 unLoginCard。 */
    fun isUnLoginPage(json: JsonObject): Boolean =
        json.arr("data").orEmpty().any { (it as? JsonObject)?.string("entityTemplate") == "unLoginCard" }

    /** 评论列表和楼中楼（replyList）。 */
    fun replyList(json: JsonObject, cursor: FeedCursor): Page<Comment> {
        val data = json.arr("data") ?: throw ParseException(P, "评论列表缺少 data 数组")
        val entities = data.mapNotNull { it as? JsonObject }
        val items = entities.filter { it.string("entityType") == "feed_reply" }.mapNotNull { comment(it) }
        return Page(items, nextCursor(entities, "feed_reply", cursor))
    }

    /**
     * 下一页：page + 1，firstItem 是第一页第一条内容的 id（之后一直带着），lastItem 是这一页最后一条内容的 id
     * （c001apk-flutter `common_controller.dart` 的做法，卡片没有 id，所以只看内容实体）。
     * 一条内容都没有（含卡片也没有）就是到头了。
     */
    private fun nextCursor(entities: List<JsonObject>, type: String, cursor: FeedCursor): Cursor? {
        val ids = entities.filter { it.string("entityType") == type }.mapNotNull { it.id() }
        if (ids.isEmpty()) return null
        return FeedCursor(cursor.page + 1, cursor.firstItem ?: ids.first(), ids.last()).encode()
    }

    // ---------- 内容 ----------

    /**
     * 一条 feed 实体。类型看 `feedType`：feedArticle → 图文（ARTICLE），question → 提问，answer → 回答，
     * video（或带视频的）→ 视频，其余（feed、comment、rating、picture……）都当帖子。
     * 标题只用 `message_title`（`title` 字段是服务端拼的"xxx的动态"，不是真标题）。
     */
    fun feed(o: JsonObject): FeedItem? {
        val id = o.id() ?: return null
        val feedType = o.text("feedType")
        val titleText = o.text("message_title")?.let { CoolapkHtml.toPlain(it) }?.takeIf { it.text.isNotBlank() }
        val summaryText = o.text("message")?.let { CoolapkHtml.toPlain(it) }?.takeIf { it.text.isNotBlank() }
        val title = titleText?.text
        val summary = summaryText?.text
        val video = video(o)
        val images = pictures(o).map { Media.Image(it.url, it.width, it.height) }
        val media = if (video != null) listOf(video) + images else images
        if (title == null && summary == null && media.isEmpty()) return null

        val url = feedUrl(id)
        return FeedItem(
            platform = P,
            id = id,
            kind = when {
                feedType == "feedArticle" -> ItemKind.ARTICLE
                feedType == "question" -> ItemKind.QUESTION
                feedType == "answer" -> ItemKind.ANSWER
                feedType == "video" || video != null -> ItemKind.VIDEO
                else -> ItemKind.POST
            },
            title = title,
            summary = summary,
            emotes = titleText?.emotes.orEmpty() + summaryText?.emotes.orEmpty(),
            author = author(o),
            publishedAt = o.long("dateline")?.takeIf { it > 0 },
            media = media,
            stats = Stats(
                likes = o.long("likenum"),
                comments = o.long("replynum"),
                shares = o.long("forwardnum"),
            ),
            // 推荐流里是"来自头条推荐""来自机型推荐"这类推荐理由
            reason = o.text("infoHtml")?.let { CoolapkHtml.toPlainText(it) }?.ifBlank { null },
            community = community(o),
            repost = (o.obj("forwardSourceFeed") ?: o.obj("sourceFeed"))?.let(::feed),
            url = url,
            detail = DetailTarget.Native(id),
        )
    }

    /**
     * 详情（/v6/feed/detail）。正文：图文（feedArticle）的 `message_raw_output` 是 JSON 字符串，
     * 里面按顺序排着 text / image / shareUrl 块（c001apk-flutter `feed_controller.dart` 的解析方式），
     * 前面再放封面；普通动态就是 message 加上 picArr 里的图片。
     */
    fun detail(json: JsonObject, resolvedMedia: String? = null): FeedDetail {
        val data = json.obj("data") ?: throw ParseException(P, "详情缺少 data")
        val item = feed(data) ?: throw ParseException(P, "详情里没有可用的内容（id=${data.id()}）")
        val article = articleBlocks(data)
        // 视频动态：酷安托管的视频是 FeedDetail.video（播放器放页面顶上）；站外视频（B站 / 微博 / 抖音……）是正文里的一张卡片
        val hosted = CoolapkVideo.hosted(data)
        val external = CoolapkVideo.externalCard(data, resolvedMedia)
        val body = if (article != null) {
            val cover = data.text("message_cover")?.let { CoolapkHtml.image(it) }
            listOfNotNull(cover) + article
        } else {
            CoolapkHtml.toBlocks(data.text("message").orEmpty()) + listOfNotNull(external) + pictures(data)
        }
        return FeedDetail(item, RichText(body), video = CoolapkVideo.videoInfo(hosted))
    }

    private fun articleBlocks(o: JsonObject): List<Block>? {
        val raw = o.text("message_raw_output") ?: return null
        val arr = runCatching { FeedJson.parseToJsonElement(raw) as? JsonArray }.getOrNull() ?: return null
        if (arr.isEmpty()) return null
        return arr.mapNotNull { it as? JsonObject }.flatMap { b ->
            when (b.string("type")) {
                "text" -> CoolapkHtml.toBlocks(b.text("message").orEmpty())
                "image" -> listOfNotNull(b.text("url")?.let { CoolapkHtml.image(it) }) +
                    listOfNotNull(b.text("description")?.let { Block.Paragraph(listOf(Span.Text(it.trim()))) })
                "shareUrl" -> listOfNotNull(
                    b.text("url")?.let { u -> Block.Paragraph(listOf(Span.Link(b.text("title") ?: u, u))) },
                )
                else -> emptyList()
            }
        }
    }

    // ---------- 评论 ----------

    /** 一条评论（一级评论或楼中楼）。[preview] 为 false 时连带解析内嵌的楼中楼预览 `replyRows`。 */
    fun comment(o: JsonObject, preview: Boolean = false): Comment? {
        val id = o.id() ?: return null
        val pics = pictures(o)
        val text = CoolapkHtml.toBlocks(o.text("message").orEmpty())
        val ruid = o.string("ruid")?.takeIf { it.isNotBlank() && it != "0" }
        return Comment(
            platform = P,
            id = id,
            author = author(o),
            content = RichText(text + pics),
            publishedAt = o.long("dateline")?.takeIf { it > 0 },
            likes = o.long("likenum"),
            replyCount = o.long("replynum"),
            replyTo = ruid?.let { UserRef(P, it, o.text("rusername") ?: it, url = CoolapkHtml.userUrl(it)) },
            ipLocation = o.text("ip_location"),
            previewReplies = if (preview) emptyList()
            else o.arr("replyRows").orEmpty().mapNotNull { (it as? JsonObject)?.let { r -> comment(r, preview = true) } },
        )
    }

    // ---------- 账号：/v6/account/checkLoginInfo ----------

    /** 只取 uid / 用户名 / 头像（响应里还有登录 token，不碰）。 */
    fun account(json: JsonObject): UserRef? {
        val data = json.obj("data") ?: return null
        val uid = data.string("uid")?.takeIf { it.isNotBlank() && it != "0" } ?: return null
        return UserRef(P, uid, data.text("username") ?: uid, data.text("userAvatar")?.toHttps(), CoolapkHtml.userUrl(uid))
    }

    // ---------- 小工具 ----------

    internal fun JsonObject.id(): String? =
        (string("id") ?: string("entityId"))?.trim()?.takeIf { it.isNotEmpty() && it != "0" }

    private fun author(o: JsonObject): UserRef? {
        val info = o.obj("userInfo")
        val uid = (info?.string("uid") ?: o.string("uid"))?.takeIf { it.isNotBlank() && it != "0" } ?: return null
        val name = info?.text("username") ?: o.text("username") ?: uid
        val avatar = (info?.text("userAvatar") ?: o.text("userAvatar"))?.toHttps()
        return UserRef(P, uid, name, avatar, CoolapkHtml.userUrl(uid))
    }

    /** 话题 / 数码产品 / 看看号：先 ttitle（主话题或机型），再 targetRow，再 relationRows 第一个。 */
    private fun community(o: JsonObject): String? =
        o.text("ttitle")?.trim()
            ?: o.obj("targetRow")?.text("title")?.trim()
            ?: o.arr("relationRows")?.firstNotNullOfOrNull { (it as? JsonObject)?.text("title")?.trim() }
            ?: o.text("dyh_name")?.trim()

    /** picArr（没有就退回 pic）。 */
    private fun pictures(o: JsonObject): List<Block.Image> {
        val arr = o.arr("picArr").orEmpty()
            .mapNotNull { (it as? JsonPrimitive)?.contentOrNull?.takeIf(String::isNotBlank) }
        val urls = arr.ifEmpty { listOfNotNull(o.text("pic")) }
        return urls.distinct().map { CoolapkHtml.image(it) }
    }

    /** 视频：media_type 不为 0 且有 media_url 或 media_pic 时算视频，封面用 media_pic（没有就用 media_info.cover），时长和宽高在 media_info 里。 */
    private fun video(o: JsonObject): Media.Video? {
        val type = o.long("media_type") ?: 0
        if (type == 0L) return null
        val info = CoolapkVideo.info(o)
        val cover = o.text("media_pic") ?: info.cover
        if (cover == null && o.text("media_url") == null) return null
        return Media.Video(cover?.toHttps(), info.durationSec, info.width, info.height)
    }
}
