package feedhub.source.tiktok

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
import feedhub.core.TimePrecision
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
 * TikTok web 响应 → 统一模型。纯函数，单测直接喂 fixtures（`TiktokParserTest`）。
 *
 * ## 结构（web 接口是 camelCase，3MH 的报告与 f2 的 filter 都印证）
 *
 * - `item_list` 类接口：`itemList[]`（每项是一个 **itemStruct**）、`cursor`、`hasMore`、`statusCode`。
 *   itemStruct 的字段：`id` / `desc` / `createTime`（秒）/ `author{id, uniqueId, nickname, avatarThumb…}` /
 *   `stats{playCount, diggCount, commentCount, shareCount, collectCount}` / `video{…}` / `imagePost{images[]}` /
 *   `textExtra[]`（话题）。
 * - 单条作品：视频页 / 详情接口给的都是同一个 **itemStruct**（[item] 一套代码两处用）。
 * - 评论：`comments[]`（web 也是 snake_case 的老字段名，见 [comment] 的兼容写法）/ `cursor` / `hasMore` / `total`。
 *
 * ## 18+ 标记：**实测没有可映射的字段**
 *
 * itemStruct 里**没有
 * `isAdult` 这类字段**：视频页那份有 `warnInfo`（正常视频是空数组），信息流那份连 `warnInfo` 都没有，
 * 只有 `isProhibited` / `item_control` / `penaltyContext`（后者是一组内容安全码，语义没出处）。
 * 首页里的 `sensitive` 字样是**光敏 / 敏感媒体提示文案**，不是成人标记。**没有 18+ 的正样本**，
 */
internal object TiktokParser {
    private val P = TIKTOK

    /** 没有识别出登录者时给用户看的地址形态（TikTok 网页的分享地址）。 */
    fun videoUrl(id: String): String = "https://www.tiktok.com/${nativeId(id)}"

    fun userUrl(uniqueId: String) = "https://www.tiktok.com/@$uniqueId"

    // ---------- id 形态 ----------

    /**
     * 详情 id：`<uniqueId>/<itemId>`（例如 `tiktok/video/7300000000000000123` 里 `tiktok` 是作者）。
     *
     * 为什么要把作者带进来：详情走的是**视频页 HTML 的 hydration**（`/@<作者>/video/<id>`，
     * 出处：TiktokApis 的 `_video_detail_from_html`、catbus 的 `videoDetail`），URL 里必须带作者；
     * 而 `FeedItem.id` 只能是**裸 itemId**（跨流去重、收藏 / 历史的 key 用它），所以作者放在
     * `DetailTarget.Native` 的 id 里。
     * 只有裸 itemId 时（cli 手工给的那种）退回 `/video/<id>` 形态，能不能开会如实在结果里写。
     */
    fun detailId(uniqueId: String?, itemId: String): String =
        if (uniqueId.isNullOrBlank()) itemId else "$uniqueId/$itemId"

    /** 详情 id → 裸 itemId（最后一段）。 */
    fun itemIdOf(id: String): String = id.substringAfterLast('/').ifBlank { id }

    /** 详情 id → 作者 uniqueId（没有就 null）。 */
    fun uniqueIdOf(id: String): String? =
        id.substringBeforeLast('/', "").takeIf { it.isNotBlank() && it != id }

    /** `/@<作者>/video/<id>` 的路径段（[nativeId] 的前面部分）。 */
    private fun nativeId(id: String): String {
        val uniqueId = uniqueIdOf(id)
        return if (uniqueId == null) "video/${itemIdOf(id)}" else "@$uniqueId/video/${itemIdOf(id)}"
    }

    // ---------- item_list 类接口 ----------

    /**
     * 一页 `item_list`。[cursor] 是响应里的游标，`hasMore` 为假结束；两者都没有时按"没有下一页"处理
     * （宁可少几页，也不要循环请求同一个游标）。
     */
    fun feed(json: JsonObject, emotes: Map<String, String> = emptyMap(), fallbackCursor: String? = null): Page<FeedItem> {
        checkStatus(json)
        val list = json.arr("itemList") ?: json.arr("item_list") ?: throw ParseException(P, "推荐流缺少 itemList")
        val items = list.mapNotNull { it.obj()?.let { o -> item(o, emotes) } }
        val hasMore = (json.bool("hasMore") ?: json.bool("has_more")) == true
        // 每回一批新的）。所以没有 cursor 时用一个每次都不同的占位游标把"还有下一页"表达出来，
        // 平台侧不用它。
        val cursor = json.long("cursor")?.toString() ?: (fallbackCursor?.takeIf { hasMore })
        return Page(items, cursor?.let(::Cursor))
    }

    /** 一页里所有文案（判断要不要去取表情、取回来挑哪几个用）。TikTok 表情表这一阶段不做，留空。 */
    fun feedTexts(json: JsonObject): String =
        json.arr("itemList").orEmpty().mapNotNull { it.obj()?.text("desc") }.joinToString("\n")

    /**
     * 一条 itemStruct → [FeedItem]。没有 id 的丢掉（广告 / 非内容卡片返回 null）。
     * [FeedItem.id] 是**裸 itemId**，[DetailTarget.Native] 里带作者（见 [detailId]）。
     */
    fun item(o: JsonObject, emotes: Map<String, String> = emptyMap()): FeedItem? {
        // 广告不是内容：丢
        if (o.bool("isAd") == true || o.bool("is_ad") == true) return null
        val id = o.text("id") ?: return null
        val author = author(o.obj("author"))
        val desc = o.text("desc")
        val images = imagePostImages(o)
        val video = o.obj("video")?.takeIf { images.isEmpty() }?.let(::videoMedia)
        val isVideo = video != null
        val stats = o.obj("stats") ?: o.obj("statsV2")
        return FeedItem(
            platform = P,
            id = id,
            kind = when {
                isVideo -> ItemKind.VIDEO
                images.isNotEmpty() -> ItemKind.POST
                else -> ItemKind.OTHER
            },
            // TikTok 没有标题字段，`desc` 是文案：放 summary，title 留 null（不编字段）
            title = null,
            summary = desc,
            author = author,
            // createTime 是秒（f2 的 filter 直接 timestamp_2_str）
            publishedAt = o.long("createTime")?.takeIf { it > 0 },
            media = listOfNotNull(video) + images,
            stats = Stats(
                views = stats?.long("playCount")?.takeIf { it > 0 },
                likes = stats?.long("diggCount"),
                comments = stats?.long("commentCount"),
                shares = stats?.long("shareCount"),
                favorites = stats?.long("collectCount"),
            ),
            repost = null,
            // 18+ 标记：这一阶段不映射（见 [adult] 与类说明）
            adult = adult(o),
            // 话题当"圈子"（textExtra[].hashtagName）
            community = firstHashtag(o),
            url = videoUrl(detailId(author?.id, id)),
            detail = DetailTarget.Native(detailId(author?.id, id)),
        )
    }

    /**
     * 18+ / 敏感内容映射。**这一阶段不映射**（含义拿不准，见类说明）：
     * 参考项目里只有 3MH 的 embed 样例出现过空的 `warnInfo`，没有 18+ 样本。
     * 真实录制回来之后如果确认了字段与语义，就在这里返回 true 并补测试。
     */
    fun adult(o: JsonObject): Boolean = false

    private fun imagePostImages(o: JsonObject): List<Media.Image> =
        o.obj("imagePost")?.arr("images").orEmpty().mapNotNull { e ->
            val img = e.obj() ?: return@mapNotNull null
            val url = img.obj("imageURL")?.urlList() ?: img.urlList() ?: return@mapNotNull null
            Media.Image(url.toHttps(), img.int("imageWidth"), img.int("imageHeight"))
        }

    private fun videoMedia(v: JsonObject): Media.Video? {
        // `video.cover` 等是**字符串**，[TiktokVideo.coverUrl] 两种形态都认
        val cover = TiktokVideo.coverUrl(v)
        // `video.duration` 是**秒**
        val durationSec = v.long("duration")?.takeIf { it > 0 }?.toInt()
        if (cover == null && durationSec == null) return null
        return Media.Video(
            cover = cover,
            durationSec = durationSec,
            width = v.int("width"),
            height = v.int("height"),
        )
    }

    private fun JsonObject.urlList(): String? =
        arr("urlList")?.firstNotNullOfOrNull { (it as? kotlinx.serialization.json.JsonPrimitive)?.content }
            ?: arr("url_list")?.firstNotNullOfOrNull { (it as? kotlinx.serialization.json.JsonPrimitive)?.content }

    /** 作者：`uniqueId` 是详情 URL 要用的，`secUid` 只用来去重（都没有就退回数字 id）。 */
    fun author(a: JsonObject?): UserRef? {
        if (a == null) return null
        val uniqueId = a.text("uniqueId") ?: a.text("unique_id")
        val secUid = a.text("secUid") ?: a.text("sec_uid")
        val uid = a.text("id") ?: a.text("uid")
        val id = uniqueId ?: secUid ?: uid ?: return null
        return UserRef(
            platform = P,
            id = id,
            name = a.text("nickname") ?: uniqueId ?: id,
            avatar = (
                a.obj("avatarThumb") ?: a.obj("avatarMedium") ?: a.obj("avatarLarger")
                    ?: a.obj("avatar_thumb") ?: a.obj("avatar_medium") ?: a.obj("avatar_larger")
                )?.urlList()?.toHttps(),
            url = uniqueId?.let(::userUrl),
        )
    }

    private fun firstHashtag(o: JsonObject): String? {
        o.arr("textExtra").orEmpty().forEach { e ->
            val name = e.obj()?.text("hashtagName") ?: e.obj()?.text("hashtag_name")
            if (!name.isNullOrBlank()) return name
        }
        return null
    }

    // ---------- 详情 ----------

    /**
     * itemStruct → [FeedDetail]（详情页和播放都用同一份数据，[id] 是 `DetailTarget.Native` 的 id）。
     *
     * 正文：视频作品的 `desc` 一个段落（话题只是文字，不做原生话题页）；图文作品把 `imagePost.images`
     * 放进正文。计数 / 作者 / 时间用同一份 [item]（保证 `FeedDetail.item.id` 和信息流里那条一致）。
     */
    fun detail(o: JsonObject, id: String, emotes: Map<String, String> = emptyMap()): FeedDetail {
        val item = item(o, emotes) ?: throw ParseException(P, "这条作品没有 id，解析不出条目")
        val desc = item.summary
        val blocks = buildList {
            if (!desc.isNullOrBlank()) add(Block.Paragraph(listOf(Span.Text(desc))))
            for (img in imagePostImages(o)) {
                add(Block.Image(img.url, img.width, img.height))
            }
        }
        return FeedDetail(
            item = item.copy(id = itemIdOf(id)),
            body = RichText(blocks),
            video = TiktokVideo.info(o),
            related = emptyList(),
        )
    }

    // ---------- 评论 ----------

    /**
     * 一级评论 / 楼中楼共用。字段名同时接受 web 的 camelCase 与老接口的 snake_case
     */
    fun comments(json: JsonObject): Page<Comment> {
        checkStatus(json)
        val list = json.arr("comments") ?: throw ParseException(P, "评论响应缺少 comments")
        val items = list.mapNotNull { it.obj()?.let(::comment) }
        val hasMore = (json.bool("hasMore") ?: json.bool("has_more")) == true
        val cursor = json.long("cursor")?.takeIf { hasMore }
        return Page(items, cursor?.let { Cursor(it.toString()) })
    }

    /** 一条评论。`reply_comment` / `replyComment` 里的预览楼中楼也解析（TikTok 一级评论常自带几条）。 */
    fun comment(o: JsonObject): Comment? {
        val id = o.text("cid") ?: o.text("commentId") ?: o.text("id") ?: return null
        val user = o.obj("user") ?: o.obj("author")
        val text = o.text("text") ?: o.text("content") ?: ""
        val previews = o.arr("reply_comment").orEmpty().mapNotNull { it.obj()?.let(::comment) }
            .ifEmpty { o.arr("replyComment").orEmpty().mapNotNull { it.obj()?.let(::comment) } }
        val replyTo = o.obj("reply_to_user")?.let(::author) ?: o.obj("replyToUser")?.let(::author)
        return Comment(
            platform = P,
            id = id,
            author = author(user),
            content = RichText.ofText(text),
            publishedAt = (o.long("create_time") ?: o.long("createTime"))?.takeIf { it > 0 },
            publishedAtPrecision = TimePrecision.SECOND,
            likes = o.long("digg_count") ?: o.long("diggCount"),
            replyCount = o.long("reply_comment_total") ?: o.long("replyCommentTotal"),
            replyTo = replyTo,
            // `stick_position` / `isPinned` 只见过 0 / false，照实映射
            pinned = (o.long("stick_position") ?: o.long("stickPosition"))?.let { it > 0 }
                ?: (o.bool("isPinned") ?: false),
            previewReplies = previews.take(2),
        )
    }

    // ---------- 登录态 / 设备（首页与视频页 HTML 的 hydration） ----------

    /**
     * 页面里 `__UNIVERSAL_DATA_FOR_REHYDRATION__` 的 `__DEFAULT_SCOPE__`。
     * 出处：TiktokApis `builder/auth.py` 的 `__UNIVERSAL_DATA_FOR_REHYDRATION__`、catbus 的 `hydration()`。
     */
    fun hydration(html: String): JsonObject? {
        val marker = "id=\"__UNIVERSAL_DATA_FOR_REHYDRATION__\""
        val start = html.indexOf(marker)
        if (start < 0) return null
        val open = html.indexOf('>', start)
        val end = html.indexOf("</script>", open).takeIf { it > 0 } ?: return null
        val raw = html.substring(open + 1, end).trim()
        return runCatching { FeedJson.parseToJsonElement(raw).obj()?.obj("__DEFAULT_SCOPE__") }.getOrNull()
    }

    /** 视频页 hydration 里的作品对象（`webapp.video-detail.itemInfo.itemStruct`）。 */
    fun videoDetail(scope: JsonObject): JsonObject? =
        scope.obj("webapp.video-detail")?.obj("itemInfo")?.obj("itemStruct")

    /** 视频页 hydration 的错误码：10204 / 10216 = 作品不存在或不可见（catbus `videoDetail`）。 */
    fun videoDetailStatus(scope: JsonObject): Pair<Long?, String?> {
        val detail = scope.obj("webapp.video-detail") ?: return null to null
        return detail.long("statusCode") to detail.text("statusMsg")
    }

    /** 首页 hydration 的 `webapp.app-context`（服务端设备数据：wid / odinId / region / webIdCreatedTime）。 */
    fun appContext(scope: JsonObject): JsonObject? = scope.obj("webapp.app-context")

    /**
     * 当前登录者。首页页面的 hydration 在几个可能的 scope 里挂账号信息（TikTok 不同版本位置不一样），
     * 这里按"最可能"的顺序找一遍；**找不到就返回 null**（宁可显示"登录已失效"，也不编一个账号）。
     */
    fun selfUser(scope: JsonObject): UserRef? {
        val candidates = listOfNotNull(
            scope.obj("webapp.user-detail")?.obj("userInfo")?.obj("user"),
            scope.obj("webapp.app-context")?.obj("user"),
            scope.obj("webapp.app-context")?.obj("userInfo")?.obj("user"),
            scope.obj("webapp.user-detail")?.obj("user"),
        )
        return candidates.firstNotNullOfOrNull { user ->
            val uniqueId = user.text("uniqueId") ?: user.text("unique_id")
            val uid = user.text("id") ?: user.text("uid")
            val id = uniqueId ?: uid ?: return@firstNotNullOfOrNull null
            UserRef(
                platform = P,
                id = id,
                // 首页 app-context 的字段拼法和大驼峰不一样：昵称是 **`nickName`**、头像是 `avatarUri`
                name = user.text("nickname") ?: user.text("nickName") ?: uniqueId ?: id,
                avatar = (
                    user.obj("avatarThumb")?.urlList() ?: user.obj("avatarLarger")?.urlList()
                        ?: user.obj("avatarUri")?.urlList()
                    )?.toHttps(),
                url = uniqueId?.let(::userUrl),
            )
        }
    }

    // ---------- 业务码 ----------

    /** `statusCode` / `status_code` 不是 0 就抛（映射见 `tiktokBusinessError`）；是 0 / 没有就原样返回。 */
    fun checkStatus(json: JsonObject): JsonObject {
        val code = json.long("statusCode") ?: json.long("status_code") ?: return json
        if (code == 0L) return json
        throw tiktokBusinessError(code, json.text("statusMsg") ?: json.text("status_msg") ?: json.text("message"))
    }
}
