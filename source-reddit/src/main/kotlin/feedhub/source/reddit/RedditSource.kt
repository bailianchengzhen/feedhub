package feedhub.source.reddit

import feedhub.core.ApiException
import feedhub.core.Comment
import feedhub.core.CommentSort
import feedhub.core.Cursor
import feedhub.core.DefaultHeadersInterceptor
import feedhub.core.FeedDetail
import feedhub.core.FeedItem
import feedhub.core.FeedKind
import feedhub.core.HttpResult
import feedhub.core.LoginSpec
import feedhub.core.MinIntervalInterceptor
import feedhub.core.NotLoggedInException
import feedhub.core.Page
import feedhub.core.ParseException
import feedhub.core.Platform
import feedhub.core.PlayInfo
import feedhub.core.RawRequest
import feedhub.core.RawResponse
import feedhub.core.RiskControlException
import feedhub.core.SourceContext
import feedhub.core.UserRef
import feedhub.core.UserAgents
import feedhub.core.VideoSource
import feedhub.core.arr
import feedhub.core.fetch
import feedhub.core.obj
import feedhub.core.parseJsonObject
import feedhub.core.text
import feedhub.core.toHttps
import feedhub.core.toOkHttpRequest
import feedhub.core.toRawResponse
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import okhttp3.HttpUrl.Companion.toHttpUrl
import okhttp3.Request

/**
 * Reddit。
 *
 * 做法：带导入的 `reddit_session` Cookie 直接请求 `www.reddit.com/<path>.json`，都加 `raw_json=1`；
 * 读操作不要 modhash。路线 A（`reddit_session` 换 bearer token + `gql-fed` 持久化查询）
 * 当前不做，接法见任务结果里的"C 不行时 A 怎么接"。
 *
 * ## 范围
 *
 * - 推荐流 [FeedKind.RECOMMEND]：`/best.json`（登录后的个人首页，"Best"）；
 * - 关注流 [FeedKind.FOLLOWING]：`/new.json`；
 * - 详情 / 评论 / 楼中楼：`/comments/<id>.json`、`/comments/<id>/_/<cid>.json`、`/api/morechildren`（都是 GET，不要 modhash）；
 * - 视频：`reddit_video` 的 DASH 清单（`DASHPlaylist.mpd`），交给现有的分片中转 + `DashManifest`；HLS 作回退（见 [RedditVideo]）；
 * - 不做的：投票 / 发帖 / 评论 / 订阅（写操作）、私信、搜索、路 A。
 *
 * ## 头像
 *
 * Reddit 的列表 / 评论 JSON **不带**作者头像，所以：
 * - **信息流卡片画版块图标**：列表加 `sr_detail=1`（不多发请求），
 *   `sr_detail.community_icon` / `icon_img` → [feedhub.core.FeedItem.communityIcon]，前端优先画它；
 * - **详情正文 / 评论区的作者头像**用按账号 id 批量取的 `GET /api/user_data_by_account_ids.json?ids=…`
 *   （一次最多 100 个），每页评论 / 每次详情只多发 1 个请求，账号在[内存里缓存][avatars]；取不到就留空，不影响评论。
 *
 * ## 登录态与风控
 *
 * - 登录态 = `.reddit.com` 下的 `reddit_session`（从 Chrome 导入，见 `CookieImportChrome` 那条路）。没登录就**不发请求**
 *   （`/…/.json` 未登录会被 403），`whoami` 返回 null。
 * - 403 / 429 / `x-ratelimit-remaining: 0` / 拿到 HTML 挑战页一律抛 [RiskControlException]，**不重试、不换 UA 再试**
 * 。403 的异常消息里带 body 开头和响应头**名字**（判断"TLS 指纹还是 IP"的证据）；
 *   绝不带 Cookie 值。
 * - 请求间隔 1000ms + 0~500ms 抖动（redlib 的观察是每分钟约 100 次，保守一些）。
 *
 * [base] / [minIntervalMs] / [jitterMs] 只给测试用（指向 MockWebServer、把间隔调成 0）。
 */
class RedditSource internal constructor(
    private val ctx: SourceContext,
    private val base: String = WWW,
    private val minIntervalMs: Long = MIN_INTERVAL_MS,
    private val jitterMs: Long = JITTER_MS,
) : VideoSource {
    constructor(ctx: SourceContext) : this(ctx, WWW)

    // 身份、流、登录、域名都以插件描述为准（[RedditPlugin]），这里不再写第二份
    override val platform = REDDIT
    override val feeds = RedditPlugin.capabilities.feeds
    override val login = RedditPlugin.login.web
    override val apiBase = base
    override val hosts = RedditPlugin.hosts

    private val baseHost = base.toHttpUrl().host

    /**
     * 作者头像的内存缓存：`t2_xxx` → 头像地址；**值是 null 表示"问过、没有"**。
     * 只在这一次运行里有效（不落盘），见 [avatars]。
     */
    private val avatarCache = java.util.Collections.synchronizedMap(HashMap<String, String?>())

    // ctx.client(platform)：套上这个平台的代理配置（Reddit 默认点亮，见 app 的 proxiedPlatforms）
    private val http = ctx.client(platform)
        .addInterceptor(
            DefaultHeadersInterceptor(
                headers(),
                hostFilter = { isRedditHost(it) || it == baseHost },
            ),
        )
        .addInterceptor(MinIntervalInterceptor(minIntervalMs, jitterMs))
        .build()

    /**
     * 平台客户端（挂好了 Reddit 的默认头 + 1000ms 限速 + 代理选择 + Cookie 罐）。
     *
     * 只给 [RustRedditSource] 用：Rust 的宿主回调必须用**同一个对象**发请求，否则限速会变成两套
     * （各自的 `MinIntervalInterceptor` 实例各有一个"下次允许的时间"），拦截器链也会多一份。
     */
    internal val httpClient get() = http

    // ---------- 登录态 ----------

    /**
     * `GET /api/me.json`：一个最便宜的登录态检查。没登录返回 null（不抛异常，[UserRef] 的约定）。
     *
     * 未登录时这个接口回 `403 {"message":"Forbidden","error":403}`；真正的拦截页是 HTML（Cloudflare 那一类），
     * 所以这里按 body 是不是 JSON 区分"没登录"和"被风控拦了"——后者抛 [RiskControlException]。
     */
    override suspend fun whoami(): UserRef? {
        if (!loggedIn()) return null
        val res = fetch("/api/me.json", listOf("raw_json" to "1"))
        if (res.code == 401) return null
        if (res.code == 403 && res.body.trimStart().startsWith("{")) return null
        checkTransport(res)
        return RedditParser.me(parseJsonObject(platform, res.body))
    }

    private fun loggedIn(): Boolean = ctx.cookies.find(WWW, SESSION_COOKIE) != null

    // ---------- 信息流 ----------

    override suspend fun feed(kind: FeedKind, cursor: Cursor?): Page<FeedItem> {
        val path = when (kind) {
            FeedKind.RECOMMEND -> BEST_PATH
            FeedKind.FOLLOWING -> NEW_PATH
        }
        requireLogin(kind)
        // sr_detail=1：让每条帖子带上 sr_detail（版块图标 community_icon / icon_img，卡片的头像位置画它），
        // 不多发请求
        val params = mutableListOf("raw_json" to "1", "limit" to PAGE_SIZE.toString(), "sr_detail" to "1")
        cursor?.value?.takeIf { it.isNotBlank() }?.let { params += "after" to it }
        val res = fetch(path, params)
        checkTransport(res)
        return RedditParser.listing(parseJsonObject(platform, res.body))
    }

    private fun requireLogin(kind: FeedKind) {
        if (!loggedIn()) {
            throw NotLoggedInException(platform, "Reddit 的${kind.label}流要登录：先在“我的”里导入 Reddit Cookie（未登录的 .json 会被 403）")
        }
    }

    // ---------- 详情、评论、楼中楼 ----------

    override suspend fun detail(id: String): FeedDetail {
        val thread = thread(id, limit = 1, sort = CommentSort.DEFAULT)
        // 正文作者头像：同一个批量接口（`/api/user_data_by_account_ids.json`），账号走内存缓存，
        // 评论页再遇到同一个人就不会重复问
        return RedditParser.detail(postOf(thread), avatars(accountIds(thread)))
    }

    /**
     * 一级评论：`GET /comments/<id>.json` 的第二个 Listing。顶层 `more` 节点成为下一页游标，
     * 翻页时走 `/api/morechildren`。
     *
     * 每页**多发 1 个**请求取作者头像（这一页所有 `author_fullname` 合成一次
     * `/api/user_data_by_account_ids.json`，一次最多 100 个；取不到就留空，不影响评论显示）。
     */
    override suspend fun comments(id: String, cursor: Cursor?, sort: CommentSort): Page<Comment> {
        if (cursor != null && cursor.value.startsWith(RedditParser.MORE_PREFIX)) {
            return morePage(id, RedditParser.moreIds(cursor.value), sort)
        }
        val thread = thread(id, limit = COMMENT_LIMIT, sort = sort)
        val listing = thread.mapNotNull { it.obj() }
            .firstOrNull { l -> l.obj("data")?.arr("children")?.any { it.obj()?.text("kind") != "t3" } == true }
            ?: return Page(emptyList(), null)
        return RedditParser.comments(listing, id, avatars(accountIds(thread)))
    }

    /**
     * 楼中楼：`GET /comments/<postId>/_/<commentId>.json`，返回那条评论下的回复（铺平的一层）。评论下的 `more` 节点同样是下一页游标。
     *
     * [commentId] 以 `more:` 开头时（[RedditParser] 里顶层 / 楼中楼 `more` 游标就是这种）直接走 `/api/morechildren`。
     */
    override suspend fun replies(id: String, commentId: String, cursor: Cursor?): Page<Comment> {
        if (commentId.startsWith(RedditParser.MORE_PREFIX)) {
            return morePage(id, RedditParser.moreIds(commentId), CommentSort.DEFAULT)
        }
        val res = fetch(
            "/comments/$id/_/$commentId.json",
            listOf("raw_json" to "1", "limit" to COMMENT_LIMIT.toString(), "sort" to sortOf(CommentSort.DEFAULT)),
        )
        checkTransport(res)
        val element = parseJsonElement(res.body)
        return RedditParser.commentThread(element, id, commentId, avatars(accountIds(element)))
    }

    /** `/comments/<id>.json`：`[0]` 是帖子、`[1]` 是评论。 */
    private suspend fun thread(id: String, limit: Int, sort: CommentSort): JsonArray {
        val res = fetch(
            "/comments/$id.json",
            listOf("raw_json" to "1", "limit" to limit.toString(), "sort" to sortOf(sort)),
        )
        checkTransport(res)
        return parseJsonElement(res.body) as? JsonArray
            ?: throw ParseException(platform, "详情不是 [帖子, 评论] 两个 Listing 的数组：${res.body.take(120)}")
    }

    private fun postOf(thread: JsonArray): JsonObject {
        for (el in thread) {
            val child = el.obj()?.obj("data")?.arr("children")?.firstOrNull()?.obj() ?: continue
            if (child.text("kind") == "t3") return child.obj("data") ?: continue
        }
        throw ParseException(platform, "详情里没有帖子")
    }

    /**
     * 展开一个 `more` 节点：`/api/morechildren` 一次最多认 100 个 id（[MAX_MORE_CHILDREN]），
     * 而顶层 `more` 节点的 `children` 实测可以有两百多个——所以只发前 100 个，剩下的做成下一页游标，
     * 前端滑到底接着取。
     */
    private suspend fun morePage(postId: String, ids: List<String>, sort: CommentSort): Page<Comment> {
        val chunk = ids.take(MAX_MORE_CHILDREN)
        val rest = ids.drop(MAX_MORE_CHILDREN)
        val json = moreChildren(postId, chunk, sort)
        val page = RedditParser.moreChildren(json, avatars(accountIds(json)))
        return Page(page.items, rest.takeIf { it.isNotEmpty() }?.let { Cursor(RedditParser.moreCursor(it)) })
    }

    /** `/api/morechildren`（GET，`api_type=json`）：展开一个 `more` 节点带的评论 id。 */
    private suspend fun moreChildren(postId: String, ids: List<String>, sort: CommentSort): JsonObject {
        if (ids.isEmpty()) return JsonObject(emptyMap())
        val res = fetch(
            "/api/morechildren",
            listOf(
                "api_type" to "json",
                "link_id" to "t3_$postId",
                "children" to ids.take(MAX_MORE_CHILDREN).joinToString(","),
                "limit_children" to "false",
                "sort" to sortOf(sort),
                "raw_json" to "1",
            ),
        )
        checkTransport(res)
        return parseJsonObject(platform, res.body)
    }

    // ---------- 作者头像（批量） ----------

    /**
     * 作者头像：`GET /api/user_data_by_account_ids.json?ids=t2_a,t2_b,…`，一次最多 100 个 id，
     * 返回每个账号的 `profile_img`（默认头像 `/static/avatars/defaults/…` 也算头像，照样显示）。
     *
     * - **每页评论 / 每次详情只多发 1 个请求**：这一页所有 `author_fullname` 合成一次查询；
     *   [MAX_AVATAR_IDS] 之外的留给下一次调用（下一页评论）再问。
     * - 缓存是**内存里的**（Source 实例一份）：同一个账号（详情正文作者 + 评论作者、翻页重复的人）只问一次；
     *   返回的表里既有这次刚取到的，也有缓存里的（[RedditParser] 拿它按 `author_fullname` 填头像）。
     * - 取不到（被删的账号、请求失败、结构不对）就留空，**不影响评论显示、不重试**；问过的（哪怕是空）
     *   也记下来，免得每次翻页都重问。
     */
    private suspend fun avatars(ids: Collection<String>): Map<String, String> {
        val all = ids.mapNotNull { it.trim().takeIf(String::isNotEmpty) }.distinct()
        val wanted = all.filterNot { avatarCache.containsKey(it) }.take(MAX_AVATAR_IDS)
        if (wanted.isNotEmpty()) {
            val json = try {
                val res = fetch(
                    "/api/user_data_by_account_ids.json",
                    listOf("raw_json" to "1", "ids" to wanted.joinToString(",")),
                )
                if (res.code in 200..299) parseJsonObject(platform, res.body) else null
            } catch (e: kotlinx.coroutines.CancellationException) {
                throw e
            } catch (_: Exception) {
                null
            }
            // 问过的都记下来（失败的记 null）：下一页 / 下一次打开同一条内容不再重问（"不重试"）
            for (id in wanted) {
                avatarCache[id] = json?.obj(id)?.text("profile_img")?.replace("&amp;", "&")?.toHttps()?.takeIf { it.isNotBlank() }
            }
        }
        return all.mapNotNull { id -> avatarCache[id]?.let { id to it } }.toMap()
    }

    /**
     * 一份响应里所有 `author_fullname`（去重）。
     *
     * **先收顶层**（帖子 + 这一页列出来的一级评论），再收楼中楼预览里的（嵌套 `replies`）：
     * [avatars] 一次只有 100 个名额，
     * 按文档顺序走会把名额耗在嵌套回复上，只有前 ~40 条一级评论有头像；先顶层就能覆盖整屏。
     */
    private fun accountIds(element: JsonElement): List<String> {
        val out = LinkedHashSet<String>()
        collectAccountIds(element, out, skipReplies = true)
        if (out.size < MAX_AVATAR_IDS) collectAccountIds(element, out, skipReplies = false)
        return out.toList()
    }

    private fun collectAccountIds(element: JsonElement, out: MutableSet<String>, skipReplies: Boolean) {
        when (element) {
            is JsonObject -> for ((k, v) in element) {
                if (skipReplies && k == "replies") continue
                if (k == "author_fullname") {
                    (v as? JsonPrimitive)?.content?.takeIf { it.isNotBlank() }?.let(out::add)
                } else {
                    collectAccountIds(v, out, skipReplies)
                }
            }
            is JsonArray -> element.forEach { collectAccountIds(it, out, skipReplies) }
            else -> Unit
        }
    }

    // ---------- 视频 ----------

    /**
     * 播放地址：**每次都重新取一次帖子**（`/comments/<id>.json`，地址会变 / 可能失效），
     * 再取 `v.redd.it` 的 `DASHPlaylist.mpd` 解析出视频轨 + 音频轨（[RedditVideo]）。清单拿不到就退回 HLS。
     */
    override suspend fun play(id: String, partId: String, quality: Int?): PlayInfo {
        if (partId != RedditVideo.MAIN) {
            throw ApiException(platform, "part", "Reddit 的视频只有一段 main，没有 $partId")
        }
        val video = RedditVideo.videoOf(postOf(thread(id, limit = 1, sort = CommentSort.DEFAULT)))
            ?: throw ApiException(platform, "no_video", "这条帖子没有 reddit_video（不是 Reddit 托管的视频，放不了）")
        val mpdUrl = RedditVideo.dashUrlOf(video)
        val mpd = mpdUrl?.let { url ->
            runCatching { fetchText(url) }.getOrNull()?.let { RedditVideo.decodeMpd(it, url) }
        }
        return RedditVideo.play(platform, video, mpd)
    }

    /** 取一份文本（清单）。非 2xx 返回 null，让调用方走回退链。 */
    private suspend fun fetchText(url: String): String? {
        val res = http.fetch(Request.Builder().url(url).header("Accept", "*/*").build())
        return res.body.takeIf { res.code in 200..299 && it.isNotBlank() }
    }

    // ---------- 调试 ----------

    /**
     * 调试用：按本平台的规则（Cookie、请求头）发任意请求，**原样**返回响应。
     * 这里刻意**不**把 403 / 挑战页转成异常：录 403 的样子（状态码、响应头、body 开头）正是判断
     * "TLS 指纹还是 IP" 时要的证据，交给 cli 的 `raw -i` 看。
     */
    override suspend fun raw(request: RawRequest): RawResponse =
        http.fetch(request.toOkHttpRequest(base)).toRawResponse()

    // ---------- 内部 ----------

    private suspend fun fetch(path: String, params: List<Pair<String, String>>): HttpResult {
        val url = (base.trimEnd('/') + path).toHttpUrl().newBuilder()
            .apply { params.forEach { (k, v) -> addQueryParameter(k, v) } }
            .build()
        return http.fetch(Request.Builder().url(url).build())
    }

    private fun parseJsonElement(body: String): JsonElement = try {
        feedhub.core.FeedJson.parseToJsonElement(body)
    } catch (e: Exception) {
        throw ParseException(platform, "响应不是合法 JSON：${body.take(120)}", e)
    }

    private fun sortOf(sort: CommentSort): String = when (sort) {
        CommentSort.DEFAULT -> "confidence"
        CommentSort.HOT -> "top"
        CommentSort.NEWEST -> "new"
    }

    /**
     * 传输层的风控信号：识别到就停下，不重试。
     * 异常消息里只放 body 开头和响应头的**名字**，不放 Cookie。
     */
    private fun checkTransport(res: HttpResult) {
        val remaining = res.headers.firstOrNull { it.first.equals("x-ratelimit-remaining", ignoreCase = true) }?.second
        if (res.code == 401) throw NotLoggedInException(platform)
        // 403 先看：它是"TLS 指纹 / IP 被拦"的直接证据，消息里要带上 body 和响应头（429 / 额度用尽另算）
        if (res.code == 403) throw blocked(res)
        if (res.code == 429) throw RiskControlException(platform, "429", "请求过于频繁（HTTP 429）")
        if (remaining?.toDoubleOrNull()?.let { it <= 0.0 } == true) {
            throw RiskControlException(platform, "rate-limit", "限流额度用尽（x-ratelimit-remaining=0）")
        }
        if (res.code !in 200..299) throw ApiException(platform, "HTTP ${res.code}", res.body.take(120))
        val head = res.body.trimStart()
        if (head.startsWith("<!") || head.startsWith("<html")) {
            throw RiskControlException(platform, "html", "拿到的是 HTML 页面而不是 JSON（风控 / 验证页）")
        }
    }

    /** 403：带上能判断风控来源的证据（body 开头 + 像风控的响应头名字）。 */
    private fun blocked(res: HttpResult): RiskControlException {
        val interesting = res.headers.map { it.first.lowercase() }.distinct().sorted()
            .filter { n -> listOf("ratelimit", "retry-after", "cf-", "block", "server", "x-", "set-cookie").any { n.contains(it) } }
        return RiskControlException(
            platform,
            "blocked",
            "HTTP 403（blocked）：body 开头 ${res.body.trim().replace(Regex("\\s+"), " ").take(140)}；" +
                "响应头 ${interesting.joinToString(",").ifEmpty { "（没有特别的）" }}",
        )
    }

    private fun headers(): Map<String, String> = linkedMapOf(
        // 和导入 Cookie 的那个浏览器一致：桌面 Chrome
        "User-Agent" to UserAgents.DESKTOP_CHROME,
        "Accept" to "application/json, text/plain, */*",
        "Accept-Language" to "en-US,en;q=0.9,zh-CN;q=0.8,zh;q=0.7",
        "Referer" to "$WWW/",
    )

    internal companion object {
        const val WWW = "https://www.reddit.com"
        const val BEST_PATH = "/best.json"
        const val NEW_PATH = "/new.json"

        /** 登录 Cookie（`.reddit.com`）。 */
        const val SESSION_COOKIE = "reddit_session"

        const val PAGE_SIZE = 50
        const val COMMENT_LIMIT = 200
        const val MAX_MORE_CHILDREN = 100

        /** `/api/user_data_by_account_ids.json` 一次最多认 100 个账号 id（超过的留给下一次调用）。 */
        const val MAX_AVATAR_IDS = 100

        /** redlib 的观察是每分钟约 100 次；保守一点，加抖动。 */
        internal const val MIN_INTERVAL_MS = 1000L
        internal const val JITTER_MS = 500L
    }
}

/**
 * [host] 是不是 Reddit 的域名：`reddit.com` / `redd.it` / `redditmedia.com` / `redditstatic.com`
 * 本身或以 `.域名` 结尾。不能只写 `endsWith("reddit.com")`——那样 `notreddit.com` 也会命中
 */
internal fun isRedditHost(host: String): Boolean {
    val h = host.lowercase().substringBefore(':')
    return REDDIT_SUFFIXES.any { h == it || h.endsWith(".$it") }
}

private val REDDIT_SUFFIXES = listOf("reddit.com", "redd.it", "redditmedia.com", "redditstatic.com")
