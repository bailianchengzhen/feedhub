package feedhub.source.reddit

import feedhub.core.Block
import feedhub.core.Comment
import feedhub.core.Cursor
import feedhub.core.DetailTarget
import feedhub.core.FeedDetail
import feedhub.core.FeedItem
import feedhub.core.ItemKind
import feedhub.core.Media
import feedhub.core.Page
import feedhub.core.ParseException
import feedhub.core.Platform
import feedhub.core.RichText
import feedhub.core.Stats
import feedhub.core.UserRef
import feedhub.core.VideoInfo
import feedhub.core.VideoPart
import feedhub.core.arr
import feedhub.core.bool
import feedhub.core.int
import feedhub.core.long
import feedhub.core.obj
import feedhub.core.string
import feedhub.core.text
import feedhub.core.toHttps
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive

/**
 * Reddit 的 Listing / Thing（`.json` 路，路线 C）→ 统一模型。纯函数，单测直接喂 fixtures。
 *
 * 结构来自 Reddit 公开的 `/…/.json` 响应（`raw_json=1`）：
 * - 列表是 `Listing`：`data.children[]`（`kind` 是 `t3`）+ 下一页游标 `data.after`；
 * - 帖子详情是**两个** Listing 的数组：`[0]` 是帖子、`[1]` 是评论；评论的回复在 `data.replies`（对象则递归，空串表示没有）；
 * - `more` 节点（`kind == "more"`）带 `children`（还没加载的评论 id，最多 100）和 `count`，
 *   顶层的那种成为下一页游标，评论里的那种由 [replies] 用 `/api/morechildren` 展开；
 * - 评论里的 `parent_id`（`t1_…`）用来还原"回复 @谁"，`stickied` 是置顶评论。
 *
 * 丢掉的东西：广告 `promoted`、置顶帖 `stickied` / `pinned`、没有 id 的东西。
 * `over_18` 映射到 [FeedItem.adult]。
 * `spoiler` 模型里没有对应字段，当前不映射（见任务结果）。
 *
 * 参考（只学字段名和结构，不抄代码）：Octonaut `Services/RedditJSONCodec.swift`（AGPL-3.0）、
 * redlib `src/utils.rs`、`src/post.rs`（AGPL-3.0）、Hydra `api/Posts.ts` / `api/PostDetail.ts`（AGPL-3.0）。
 */
internal object RedditParser {
    private val P = REDDIT

    /** 摘要最长多少（信息流卡片只显示一行；和贴吧 `SUMMARY_LEN` 一个意思，别把长文整段塞进 FeedItem）。 */
    const val SUMMARY_LEN = 200

    /** 取摘要前先截这么多字符原文：摘要只有 200 字，4096 足够覆盖第一个 block 的边界情况。 */
    const val SUMMARY_SOURCE_LEN = 4096

    /** 楼中楼预览最多几条（前端只画 3 条）。 */
    private const val PREVIEW_MAX = 3

    /** `more` 游标的前缀：`more:<id1>,<id2>,…`。base36 的 id 不含 ':' 和 ','，不会歧义。 */
    const val MORE_PREFIX = "more:"

    fun moreCursor(ids: List<String>): String = MORE_PREFIX + ids.joinToString(",")

    fun moreIds(cursor: String): List<String> =
        cursor.removePrefix(MORE_PREFIX).split(',').map { it.trim() }.filter { it.isNotEmpty() }

    // ---------- 列表 ----------

    /**
     * 一页列表：`data.children[]` + `data.after`。
     *
     * [avatars] 是作者头像表（`author_fullname` → 头像地址，见 [RedditSource] 的
     * `/api/user_data_by_account_ids.json` 批量取法）；信息流卡片不画作者头像（画版块图标，见 [communityIcon]），
     * 所以那里传空表、一个请求都不多发。
     */
    fun listing(json: JsonObject, avatars: Map<String, String> = emptyMap()): Page<FeedItem> {
        val data = json.obj("data") ?: throw ParseException(P, "Listing 缺少 data")
        val items = data.arr("children").orEmpty().mapNotNull { child ->
            val o = child.obj()
            if (o?.text("kind") != "t3") null else o.obj("data")?.let { post(it, avatars) }
        }
        val after = data.text("after")
        return Page(items, after?.takeIf { it.isNotBlank() }?.let(::Cursor))
    }

    // ---------- 帖子 ----------

    /**
     * 一条帖子（`t3` 的 `data`）→ [FeedItem]。广告 / 置顶 / 没有 id 的返回 null。
     *
     * 跨帖（crosspost）：外层的标题、作者、分数是转的人给的，原帖内容走 [FeedItem.repost]；
     * 外层自己没有媒体时借原帖的媒体，卡片上才画得出图（前端只在详情页画 `repost`，信息流卡片看 `media`）。
     */
    fun post(o: JsonObject, avatars: Map<String, String> = emptyMap()): FeedItem? {
        if (o.bool("promoted") == true) return null
        if (o.bool("stickied") == true || o.bool("pinned") == true) return null
        val id = o.text("id") ?: return null
        val parent = o.arr("crosspost_parent_list")?.firstOrNull()?.obj()?.takeIf { it.text("id") != null }
        val video = RedditVideo.videoOf(o)
        val own = media(o, video)
        val media = own.ifEmpty { parent?.let { media(it, RedditVideo.videoOf(it)) }.orEmpty() }
        val selftext = o.string("selftext").orEmpty()
        // 摘要只取前 SUMMARY_LEN 字，先截原文再解析 markdown；长文（几千字）不用整段走一遍 Block / Span。
        val summarySource = if (selftext.length > SUMMARY_SOURCE_LEN) selftext.take(SUMMARY_SOURCE_LEN) else selftext
        val summary = RedditMarkdown.toPlain(summarySource).take(SUMMARY_LEN).ifEmpty { null }
        val permalink = o.text("permalink")
        return FeedItem(
            platform = P,
            id = id,
            kind = if (video != null) ItemKind.VIDEO else ItemKind.POST,
            title = o.text("title"),
            summary = summary,
            author = author(o, avatars),
            publishedAt = o.long("created_utc"),
            media = media,
            stats = Stats(
                // Reddit 不给浏览量，就不编
                likes = o.long("score") ?: o.long("ups"),
                comments = o.long("num_comments"),
            ),
            community = community(o),
            communityIcon = communityIcon(o),
            repost = parent?.let { post(it, avatars) },
            url = permalink?.let { WWW + it },
            detail = DetailTarget.Native(id),
            adult = o.bool("over_18") == true,
        )
    }

    /**
     * 版块图标：`sr_detail.community_icon`（列表请求带 `sr_detail=1` 时才有），没有就退回 `sr_detail.icon_img`。
     * 两个字段都可能是 `null` / 空串。`raw_json=1` 下 `&` 已经是原样，这里再兜一次 `&amp;`。
     */
    fun communityIcon(o: JsonObject): String? {
        val sr = o.obj("sr_detail") ?: return null
        val url = sr.text("community_icon") ?: sr.text("icon_img") ?: return null
        return url.replace("&amp;", "&").toHttps().takeIf { it.isNotBlank() }
    }

    /** 帖子详情（`/comments/<id>.json` 的 `[0].data.children[0].data`）。 */
    fun detail(o: JsonObject, avatars: Map<String, String> = emptyMap()): FeedDetail {
        val item = post(o, avatars) ?: throw ParseException(P, "帖子缺少 id")
        val video = RedditVideo.videoOf(o)
        return FeedDetail(
            item = item,
            body = RichText(body(o, video)),
            video = video?.let {
                VideoInfo(parts = listOf(VideoPart(RedditVideo.MAIN, null, it.durationSec, it.width, it.height)))
            },
        )
    }

    /** 正文：selftext（markdown）+ 图片 / 相册 + 站外链接卡片。 */
    private fun body(o: JsonObject, video: RedditVideo.Video?): List<Block> {
        val blocks = mutableListOf<Block>()
        val selftext = o.string("selftext").orEmpty()
        if (selftext.isNotBlank()) blocks += RedditMarkdown.toBlocks(selftext)
        // 整条就是视频的帖子：视频在 FeedDetail.video，封面是它的海报，正文不放图片
        val images = if (video == null) imageBlocks(o) else emptyList()
        blocks += images
        val dest = o.text("url_overridden_by_dest") ?: o.string("url")
        val linkOnly = video == null && images.isEmpty() && o.bool("is_self") != true && dest != null
        if (linkOnly) {
            blocks += Block.Card(
                title = o.text("title") ?: dest,
                subtitle = o.text("domain"),
                cover = previewImage(o)?.url,
                url = dest,
            )
        }
        return blocks
    }

    // ---------- 媒体 ----------

    private fun media(o: JsonObject, video: RedditVideo.Video?): List<Media> {
        if (video != null) {
            return listOf(
                Media.Video(
                    cover = previewImage(o)?.url,
                    durationSec = video.durationSec,
                    width = video.width,
                    height = video.height,
                ),
            )
        }
        galleryImages(o)?.let { if (it.isNotEmpty()) return it.map { m -> Media.Image(m.url, m.width, m.height) } }
        previewImage(o)?.let { return listOf(Media.Image(it.url, it.width, it.height)) }
        return emptyList()
    }

    private class Img(val url: String, val width: Int?, val height: Int?, val original: String? = null, val caption: String? = null)

    /** 相册：`gallery_data.items[]` 给顺序，`media_metadata[media_id].s` 给地址和尺寸（`status` 不是 valid 的丢掉）。 */
    private fun galleryImages(o: JsonObject): List<Img>? {
        if (o.bool("is_gallery") != true) return null
        val items = o.obj("gallery_data")?.arr("items") ?: return null
        val meta = o.obj("media_metadata") ?: return null
        return items.mapNotNull { item ->
            val it = item.obj() ?: return@mapNotNull null
            val mediaId = it.text("media_id") ?: return@mapNotNull null
            val m = meta.obj(mediaId) ?: return@mapNotNull null
            if (m.text("status")?.lowercase() != "valid") return@mapNotNull null
            val source = m.obj("s")
            // `s.u` 是最大的那档（`preview.redd.it` 上的 webp / pjpg）；动图的真身看 `s.gif`
            val url = (source?.text("gif") ?: source?.text("u"))?.toHttps() ?: return@mapNotNull null
            Img(
                url = url,
                width = source?.int("x"),
                height = source?.int("y"),
                original = originalOf(url),
                caption = it.text("caption"),
            )
        }
    }

    /** 单图：`preview.images[0].source`（`preview.redd.it`）。原图能给就给（`i.redd.it` 同路径）。 */
    private fun previewImage(o: JsonObject): Img? {
        val source = o.obj("preview")?.arr("images")?.firstOrNull()?.obj()?.obj("source") ?: return null
        val url = source.text("url")?.toHttps() ?: return null
        return Img(url, source.int("width"), source.int("height"), originalOf(url))
    }

    /**
     * `preview.redd.it/<hash>.jpg?…` 的原图是 `i.redd.it/<hash>.jpg`（Reddit 自己的图床同路径，去掉缩放参数）。
     * **`external-preview.redd.it` 不算**：那是站外图片的再托管副本，`i.redd.it/<hash>` 上并没有这张图
     * 。别的域名、路径不像单段文件名时也给 null。
     */
    private fun originalOf(url: String): String? {
        val host = url.substringAfter("//").substringBefore('/')
        if (host != "preview.redd.it") return null
        val path = url.substringAfter("//").substringAfter('/').substringBefore('?')
        if (path.isEmpty() || path.contains('/')) return null
        if (!IMAGE_EXT.contains(path.substringAfterLast('.', "").lowercase())) return null
        return "https://i.redd.it/$path"
    }

    private fun imageBlocks(o: JsonObject): List<Block.Image> {
        val gallery = galleryImages(o)
        val imgs = gallery ?: listOfNotNull(previewImage(o))
        return imgs.map { Block.Image(url = it.url, width = it.width, height = it.height, original = it.original, caption = it.caption) }
    }

    // ---------- 评论 ----------

    /**
     * 一页一级评论。顶层 `more` 节点成为下一页游标（`/api/morechildren`）。
     *
     * [avatars]：作者头像表（`author_fullname` → 地址），由 [RedditSource] 每页批量取一次（最多 1 个请求）。
     */
    fun comments(listing: JsonObject, postId: String, avatars: Map<String, String> = emptyMap()): Page<Comment> {
        val data = listing.obj("data") ?: return Page(emptyList(), null)
        val items = mutableListOf<Comment>()
        val more = mutableListOf<String>()
        for (child in data.arr("children").orEmpty()) {
            val o = child.obj() ?: continue
            val d = o.obj("data") ?: continue
            when (o.text("kind")) {
                "t1" -> comment(d, avatars)?.let { items += it }
                "more" -> more += idsOf(d)
            }
        }
        return Page(items, more.takeIf { it.isNotEmpty() }?.let { Cursor(moreCursor(it)) })
    }

    /**
     * `replies()`：`/comments/<postId>/_/<commentId>.json` 的响应。形态有两种（数组 / 单个 Listing，
     * 见 [RedditSource.replies] 的说明），这里都认：先在里面找到 `id == targetId` 的评论，
     * 用它的 `replies` 作为这一页的内容；找不到就退回"最后一个 Listing 的 children"。
     *
     * 返回的是**铺平**的一层（和别的平台的楼中楼一样）：楼中楼里的嵌套回复也逐条列出，
     * `replyTo` 用 `parent_id` 在本次响应里查到的人名。
     */
    fun commentThread(json: JsonElement, postId: String, targetId: String, avatars: Map<String, String> = emptyMap()): Page<Comment> {
        val listings = when (json) {
            is JsonArray -> json.mapNotNull { it.obj() }
            is JsonObject -> listOf(json)
            else -> emptyList()
        }
        val all = mutableListOf<JsonObject>()
        listings.forEach { collectComments(it, all) }
        val byId = all.mapNotNull { d -> d.text("id")?.let { id -> id to d } }.toMap()
        val target = byId[targetId]
        val source = target?.let { t -> t.obj("replies")?.obj("data")?.arr("children")?.mapNotNull { it.obj() } }
            ?: listings.lastOrNull()?.obj("data")?.arr("children")?.mapNotNull { it.obj() }
            ?: return Page(emptyList(), null)

        val out = mutableListOf<Comment>()
        val more = mutableListOf<String>()
        for (o in source) {
            val d = o.obj("data") ?: continue
            when (o.text("kind")) {
                "t1" -> flatten(d, byId, targetId, avatars, out)
                "more" -> more += idsOf(d)
            }
        }
        return Page(out, more.takeIf { it.isNotEmpty() }?.let { Cursor(moreCursor(it)) })
    }

    /** `/api/morechildren` 的响应：`json.data.things[]`（扁平，带 `depth` / `parent_id`）。 */
    fun moreChildren(json: JsonObject, avatars: Map<String, String> = emptyMap()): Page<Comment> {
        val things = json.obj("json")?.obj("data")?.arr("things") ?: return Page(emptyList(), null)
        val all = mutableListOf<JsonObject>()
        things.forEach { thing ->
            val d = thing.obj()?.obj("data") ?: return@forEach
            collectOne(d, all)
        }
        val byId = all.mapNotNull { d -> d.text("id")?.let { id -> id to d } }.toMap()
        val items = things.mapNotNull { thing ->
            val o = thing.obj() ?: return@mapNotNull null
            if (o.text("kind") != "t1") return@mapNotNull null
            val d = o.obj("data") ?: return@mapNotNull null
            comment(d, byId, parentIsBatchRoot = true, avatars = avatars) { it }
        }
        return Page(items, null)
    }

    /** 一条评论（含楼中楼预览、回复总数、置顶）。没有 id 返回 null。 */
    private fun comment(d: JsonObject, avatars: Map<String, String>): Comment? =
        comment(d, emptyMap(), parentIsBatchRoot = false, avatars = avatars) { it }

    private fun comment(
        d: JsonObject,
        byId: Map<String, JsonObject>,
        parentIsBatchRoot: Boolean,
        avatars: Map<String, String>,
        transform: (Comment) -> Comment,
    ): Comment? {
        val id = d.text("id") ?: return null
        val author = author(d, avatars)
        val children = d.obj("replies")?.obj("data")?.arr("children").orEmpty()
        val previews = mutableListOf<Comment>()
        var childrenCount = 0L
        var moreCount = 0L
        for (child in children) {
            val o = child.obj() ?: continue
            val cd = o.obj("data") ?: continue
            when (o.text("kind")) {
                "t1" -> {
                    childrenCount++
                    if (previews.size < PREVIEW_MAX) comment(cd, byId, false, avatars) { it }?.let { previews += it }
                }
                "more" -> moreCount += cd.long("count") ?: 0
            }
        }
        val parentId = d.text("parent_id")
        val parentCommentId = parentId?.takeIf { it.startsWith("t1_") }?.removePrefix("t1_")
        val replyTo = if (parentCommentId != null && parentIsBatchRoot) byId[parentCommentId]?.let { author(it, avatars) } else null
        return transform(
            Comment(
                platform = P,
                id = id,
                author = author,
                content = RichText(RedditMarkdown.toBlocks(d.string("body"))),
                publishedAt = d.long("created_utc"),
                likes = d.long("score"),
                replyCount = (childrenCount + moreCount).takeIf { it > 0 },
                replyTo = replyTo,
                pinned = d.bool("stickied") == true,
                previewReplies = previews,
            ),
        )
    }

    /** 把 [d] 及其嵌套回复逐条铺平（楼中楼面板用），`replyTo` 在 [byId] 里查。 */
    private fun flatten(d: JsonObject, byId: Map<String, JsonObject>, targetId: String, avatars: Map<String, String>, out: MutableList<Comment>) {
        val id = d.text("id") ?: return
        if (out.any { it.id == id }) return
        val parentId = d.text("parent_id").orEmpty().removePrefix("t1_")
        val parent = byId[parentId]
        val c = comment(d, byId, parentIsBatchRoot = true, avatars = avatars) { base ->
            base.copy(
                // 直接回目标评论的不标"回复 xxx"（面板上那条就是它）；回复别人的才标
                replyTo = if (parentId == targetId || parentId.isEmpty()) null else parent?.let { author(it, avatars) },
                previewReplies = emptyList(),
            )
        } ?: return
        out += c
        // 嵌套的子回复（同一份响应里内联的那些）跟着铺平
        for (child in d.obj("replies")?.obj("data")?.arr("children").orEmpty()) {
            val o = child.obj() ?: continue
            if (o.text("kind") != "t1") continue
            o.obj("data")?.let { flatten(it, byId, targetId, avatars, out) }
        }
    }

    private fun collectComments(listing: JsonObject, out: MutableList<JsonObject>) {
        for (child in listing.obj("data")?.arr("children").orEmpty()) {
            val o = child.obj() ?: continue
            if (o.text("kind") != "t1") continue
            o.obj("data")?.let { collectOne(it, out) }
        }
    }

    private fun collectOne(d: JsonObject, out: MutableList<JsonObject>) {
        if (out.any { it.text("id") == d.text("id") }) return
        out += d
        for (child in d.obj("replies")?.obj("data")?.arr("children").orEmpty()) {
            val o = child.obj() ?: continue
            if (o.text("kind") != "t1") continue
            o.obj("data")?.let { collectOne(it, out) }
        }
    }

    /** `more` 节点的 `children`（还没加载的评论 id）。 */
    private fun idsOf(d: JsonObject): List<String> =
        d.arr("children").orEmpty().mapNotNull { (it as? JsonPrimitive)?.content?.takeIf { s -> s.isNotBlank() } }

    // ---------- 登录态 ----------

    /** `/api/me.json`：`{"kind":"t2","data":{…}}`（有的形态直接就是 `data`）。没登录返回 null。 */
    fun me(json: JsonObject): UserRef? {
        val d = json.obj("data") ?: json
        val name = d.text("name") ?: return null
        val id = d.text("id") ?: name
        return UserRef(
            platform = P,
            id = id,
            name = name,
            avatar = (d.text("icon_img") ?: d.text("snoovatar_img"))?.toHttps(),
            url = "https://www.reddit.com/user/$name",
        )
    }

    // ---------- 小工具 ----------

    /**
     * 作者。`author_fullname`（`t2_…`）是账号 id，[UserRef.id] 用去掉 `t2_` 的那段；
     * 头像地址从 [avatars]（`author_fullname` → 地址）里查——Reddit 的列表 / 评论 JSON **不带**头像，
     * 要另外用 `/api/user_data_by_account_ids.json` 按账号 id 批量取（见 [RedditSource]）。
     * 取不到（被删的账号、那一批没返回）就留空，不影响名字 / 正文。
     */
    fun author(o: JsonObject, avatars: Map<String, String> = emptyMap()): UserRef? {
        val name = o.text("author") ?: return null
        if (name == "[deleted]") return null
        val full = o.text("author_fullname")?.takeIf { it.isNotBlank() }
        return UserRef(
            platform = P,
            id = full?.removePrefix("t2_")?.takeIf { it.isNotBlank() } ?: name,
            name = name,
            avatar = full?.let { avatars[it] },
            url = "https://www.reddit.com/user/$name",
        )
    }

    fun community(o: JsonObject): String? =
        o.text("subreddit_name_prefixed") ?: o.text("subreddit")?.let { "r/$it" }

    private const val WWW = "https://www.reddit.com"

    private val IMAGE_EXT = setOf("jpg", "jpeg", "png", "gif", "webp")
}
