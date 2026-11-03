package feedhub.app.api

import feedhub.app.AppGraph
import feedhub.app.contentFilter
import feedhub.app.library.FavoriteQuery
import feedhub.app.library.HistoryQuery
import feedhub.brain.AdultFilter
import feedhub.core.Comment
import feedhub.core.CommentSort
import feedhub.core.Cursor
import feedhub.core.FeedItem
import feedhub.core.FeedJson
import feedhub.core.NativeDetailSource
import feedhub.core.Page
import feedhub.core.Platform
import feedhub.core.RequestScope
import feedhub.core.SearchKind
import feedhub.core.SearchRequest
import feedhub.core.SearchSort
import feedhub.core.SearchSource
import feedhub.brain.SearchMerge
import feedhub.brain.SearchPlan
import feedhub.rust.RustAppApi
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withContext
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.buildJsonArray
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put
import java.time.LocalDate
import java.time.LocalDateTime
import java.time.ZoneId

/**
 * 对外接口的**路由**：`/capabilities`、`/platforms`、`/search`、`/detail`、`/image`、`/comments`、`/replies`、
 * `/library/favorites`、`/library/history`。
 *
 * 列表最后一行 `type=done`。
 * 2. **请求全走这个 App 的 [AppGraph]**：`graph.source(p)` 就是界面上那个 Source（同一份 Cookie / 代理 /
 *    限速 / 风控处理），搜索规则用 core 的 `SearchPlan` / `SearchMerge`（和 App 搜索页同一份），
 *    收藏 / 历史读 App 自己的 `Library`；**接口自己不建任何东西、不记任何状态**。
 * 3. **只读**：没有登录、关注、raw、任何写操作。风控时照 App 的规矩停下、回 `risk_control`，不重试。
 *
 * `limit` 截断之后 `done.next` 给的是**可以接着往下拿的续页游标**（[ResumeCursor]）。
 *
 * 新端点 `/image` 由 App 去取图（[ApiImageService]）；`/image` 要用的图片清单在 [ApiImageIndex] 里留一份。
 */
internal class ApiRoutes(
    private val graph: () -> AppGraph,
    /** 在听时 socket 的路径（`/capabilities` 里自描述用）。 */
    private val socketPath: () -> String?,
    /** `/image` 用哪份图片加载器（默认 = App 自己那个 Coil 单例，和界面共用磁盘缓存）。 */
    private val imageLoader: () -> coil3.ImageLoader = {
        coil3.SingletonImageLoader.get(coil3.PlatformContext.INSTANCE)
    },
) {
    /** `/image` 的图片清单备忘录（只在内存里，见 [ApiImageIndex]）。 */
    private val imageIndex = ApiImageIndex()

    /** 取图 / 缩图 / 出 JPEG。 */
    private val imageService = ApiImageService(imageLoader)

    /** 处理一个请求；任何异常都变成"错误码 + 一行 JSON + 对应状态码"。 */
    fun handle(request: ApiRequest): ApiReply {
        // 这次调用自己的请求计数：放进协程上下文，App 自己刷流不会算进来（见 RequestScope）
        val call = RequestScope()
        return try {
            runBlocking { withContext(call) { route(graph(), request, call) } }
        } catch (e: Throwable) {
            failureReply(e.toApiFailure(), call)
        }
    }

    /** 失败响应：`type=error` 一行，**也带 `requests`**（agent 核预算时失败的那次也得算）。 */
    private fun failureReply(failure: ApiFailure, call: RequestScope): ApiReply =
        ApiReply(failure.error.httpStatus, failure.toJsonLine(call.requests) + "\n", failure.error.code)

    private suspend fun route(g: AppGraph, request: ApiRequest, call: RequestScope): ApiReply = when (request.path) {
        // `/` 也给 capabilities：agent 常常先 curl 一下根
        "/", "/capabilities" -> capabilities(g)
        "/platforms" -> platforms(g, call)
        "/search" -> search(g, request, call)
        "/detail" -> detail(g, request, call)
        "/image" -> image(g, request, call)
        "/comments" -> comments(g, request, call, replies = false)
        "/replies" -> comments(g, request, call, replies = true)
        "/library/favorites" -> favorites(g, request, call)
        "/library/history" -> history(g, request, call)
        else -> throw ApiFailure(
            ApiError.NOT_SUPPORTED,
            "没有这个路径：${request.path}（接口清单见 /capabilities）",
        )
    }

    // ---------------------------------------------------------------- 自描述 / 平台

    /**
     * `/capabilities`：agent 自己发现能干什么（一行 `type=capabilities`，完全离线：不建 Source、不发请求）。
     *
     * 和 cli 的 `capabilities --json` 是**同一个形状**（commands / errorCodes / platforms / limits），
     * 只是 `commands` 换成了 HTTP 的 `endpoints`（有方法、路径、参数），并多了 `output` / `socket`。
     */
    private fun capabilities(g: AppGraph): ApiReply {
        val platformJson = g.platforms.plugins.map { plugin ->
            buildJsonObject {
                put("id", JsonPrimitive(plugin.id))
                put("label", JsonPrimitive(plugin.label))
                put("feeds", jsonArrayOfStrings(plugin.capabilities.feeds.map { it.key }))
                plugin.capabilities.search?.let { cap ->
                    put(
                        "search",
                        buildJsonObject {
                            put("kinds", jsonArrayOfStrings(cap.kinds.map { it.name.lowercase() }))
                            put("sorts", jsonArrayOfStrings(cap.sorts.map { it.name.lowercase() }))
                            put("pageSize", JsonPrimitive(cap.pageSize))
                            put("needsLogin", JsonPrimitive(cap.needsLogin))
                        },
                    )
                }
                put("commentSort", JsonPrimitive(plugin.capabilities.commentSort))
                put("nestedReplies", JsonPrimitive(plugin.capabilities.nestedReplies))
                put("playableVideo", JsonPrimitive(plugin.capabilities.playableVideo))
                put("overseas", JsonPrimitive(plugin.overseas))
                put("desktop", JsonPrimitive(plugin.supports(feedhub.core.plugin.PluginTarget.DESKTOP)))
                put("available", JsonPrimitive(g.available(plugin.platform)))
                g.unavailable[plugin.platform]?.let { put("unavailableNote", JsonPrimitive(it)) }
            }
        }
        val platformsJsonStr = FeedJson.encodeToString(kotlinx.serialization.builtins.ListSerializer(JsonObject.serializer()), platformJson)
        val body = RustAppApi.renderCapabilities(socketPath() ?: "", platformsJsonStr)
        return ok(body)
    }

    /** `/platforms`：装了哪些平台、各支持什么（离线，不建 Source）。 */
    private suspend fun platforms(g: AppGraph, call: RequestScope): ApiReply {
        val platformJson = g.platforms.plugins.map { plugin ->
            buildJsonObject {
                put("id", JsonPrimitive(plugin.id))
                put("label", JsonPrimitive(plugin.label))
                put("feeds", jsonArrayOfStrings(plugin.capabilities.feeds.map { it.key }))
                plugin.capabilities.search?.let { cap ->
                    put(
                        "search",
                        buildJsonObject {
                            put("kinds", jsonArrayOfStrings(cap.kinds.map { it.name.lowercase() }))
                            put("sorts", jsonArrayOfStrings(cap.sorts.map { it.name.lowercase() }))
                            put("pageSize", JsonPrimitive(cap.pageSize))
                            put("needsLogin", JsonPrimitive(cap.needsLogin))
                        },
                    )
                }
                put("commentSort", JsonPrimitive(plugin.capabilities.commentSort))
                put("nestedReplies", JsonPrimitive(plugin.capabilities.nestedReplies))
                put("overseas", JsonPrimitive(plugin.overseas))
                put("desktop", JsonPrimitive(plugin.supports(feedhub.core.plugin.PluginTarget.DESKTOP)))
                put("available", JsonPrimitive(g.available(plugin.platform)))
            }
        }
        val platformsJsonStr = FeedJson.encodeToString(kotlinx.serialization.builtins.ListSerializer(JsonObject.serializer()), platformJson)
        return ok(RustAppApi.renderPlatforms(platformsJsonStr, call.requests))
    }

    // ---------------------------------------------------------------- 搜索

    /**
     * `/search`：`platform=all` 时和 App 搜索页**同一套规则**（`SearchPlan` / `SearchMerge`）；
     * 单平台时直接问这一家、按 `page` / `cursor` 翻。
     *
     * `all` 里某几家失败不中断：失败的那几家各多一行 `type=error`（正文里，和 `done.failed` 里那一份对得上），
     * 其余照常输出；只有**所有平台都失败**时才整体报错（和 cli 一样）。
     *
     * `limit` 把混排结果截断时 `done.truncated=true`，`done.next` 换成**续页游标**（见 [ResumeCursor]）：
     * 单平台直接带回来就能接着拿；`all` 按 `done.next` 里每一家的值、用 `platform=<那家>&cursor=<值>` 分别续
     * （`cursor` 本来就只能和单个平台一起用）。
     */
    private suspend fun search(g: AppGraph, request: ApiRequest, call: RequestScope): ApiReply {
        val params = try {
            RustAppApi.validateSearchParams(
                q = request.param("q") ?: request.param("query"),
                platform = request.param("platform"),
                only = request.param("only"),
                cursor = request.param("cursor"),
                page = request.param("page"),
                pages = request.param("pages"),
                kind = request.param("kind"),
                sort = request.param("sort"),
                limit = request.param("limit"),
            )
        } catch (e: RustAppApi.ProtocolException) {
            throw ApiFailure(ApiError.BAD_INPUT, e.detail)
        }
        val query = params.query
        val platformKey = params.platform
        val isAll = platformKey.equals("all", ignoreCase = true)
        val kindText = params.kind
        val sortText = params.sort
        val community = request.param("community")
        val kind = enumOf<SearchKind>(kindText, "kind")
        val sort = enumOf<SearchSort>(sortText, "sort")
        val only = params.only
        val limit = params.limit
        val page = params.page
        val pages = params.pages
        val cursorRaw = request.param("cursor")
        val start = ResumeCursor.decode(cursorRaw)
        val wanted = if (pages != null) (1..pages).toList() else listOf(page)
        val deepest = wanted.max()

        val settings = g.settings.value
        val available = g.platforms.platforms.filterTo(LinkedHashSet()) { g.available(it) }
        val chosen: List<Platform> = if (isAll) {
            val allow = if (only.isEmpty()) {
                settings.homePlatforms
            } else {
                // 显式点名的平台也必须在首页点亮：all 的规则是"只搜点亮的"
                for (key in only) {
                    val p = platformOf(g, key)
                    if (p !in settings.homePlatforms) {
                        throw ApiFailure(
                            ApiError.BAD_INPUT,
                            "${p.label}（${p.key}）不在首页点亮的平台里（all 只搜点亮的）；" +
                                "要么在 App 里点亮它，要么直接 search?platform=${p.key}",
                        )
                    }
                }
                only.mapTo(LinkedHashSet()) { platformOf(g, it) }
            }
            SearchPlan.platforms(g.platforms, allow, available)
        } else {
            listOf(platformOf(g, platformKey))
        }
        if (chosen.isEmpty()) {
            throw ApiFailure(
                ApiError.BAD_INPUT,
                "没有可搜的平台：首页亮着的平台里没有一家接了搜索（见 /platforms 的 search 字段）",
            )
        }

        val order = ArrayList<Platform>()
        val candidates = ArrayList<Pair<Platform, SearchSource>>()
        val failures = LinkedHashMap<Platform, ApiFailure>()
        for (p in chosen) {
            val plugin = g.platforms[p]
                ?: throw ApiFailure(ApiError.NOT_SUPPORTED, "${p.label}（${p.key}）的插件没装", p)
            val cap = plugin.capabilities.search
            val reason = when {
                cap == null -> "${p.label} 没有接搜索（描述里 capabilities.search 是 null）"
                kind !in cap.kinds -> "${p.label} 不支持 kind=$kindText"
                sort !in cap.sorts -> "${p.label} 不支持 sort=$sortText"
                else -> null
            }
            if (reason != null) {
                if (!isAll) throw ApiFailure(ApiError.NOT_SUPPORTED, reason, p)
                failures[p] = ApiFailure(ApiError.NOT_SUPPORTED, "$reason，跳过", p)
                continue
            }
            if (g.sourceOrNull(p) !is SearchSource) {
                throw ApiFailure(ApiError.NOT_SUPPORTED, "${p.label} 的描述里有搜索能力，但 Source 没实现 SearchSource", p)
            }
            order += p
            candidates += p to (g.source(p) as SearchSource)
        }
        if (candidates.isEmpty()) {
            throw ApiFailure(ApiError.BAD_INPUT, "没有可搜索的平台（每家都不支持或不可用）")
        }

        // 一家一家串行取（和 cli 一样温和；App 搜索页那边才是并发 + 超时，那些是界面行为）
        val searchRequest = SearchRequest(query, kind, sort, community)
        // 每页每家"这一段"（过滤 + 配额之后）和"这一页是从哪个游标取的"：续页游标要用
        val slices = mutableMapOf<Int, MutableMap<Platform, PageSlice>>()
        val pageStarts = mutableMapOf<Int, MutableMap<Platform, Cursor?>>()
        val nextByPlatform = LinkedHashMap<Platform, Cursor?>()
        var hidden = 0
        var answered = 0
        for ((p, source) in candidates) {
            val blocked = g.proxyBlockedReason(p)
            if (blocked != null) {
                if (!isAll) throw ApiFailure(ApiError.PROXY_UNAVAILABLE, blocked, p)
                failures[p] = ApiFailure(ApiError.PROXY_UNAVAILABLE, blocked, p)
                nextByPlatform[p] = null
                continue
            }
            try {
                var cur: Cursor? = start.platform
                var pageNo = 1
                while (pageNo <= deepest) {
                    pageStarts.getOrPut(pageNo) { mutableMapOf() }[p] = cur
                    val result = source.search(searchRequest, cur)
                    val kept = SearchPlan.keep(result.items, settings.contentFilter)
                    hidden += result.items.size - kept.size
                    if (kept.isNotEmpty()) answered++
                    if (pageNo in wanted) {
                        // 续页游标里的"跳过几条"只作用在带它回来的那一页（本次取的第一页）
                        val skip = if (pageNo == 1) start.skip else 0
                        val quota = if (candidates.size > 1) SearchPlan.quota(candidates.size, source.search.pageSize) else kept.size
                        slices.getOrPut(pageNo) { mutableMapOf() }[p] = PageSlice(kept.drop(skip).take(quota), skip)
                    }
                    cur = result.next
                    if (cur == null) break
                    if (pageNo == deepest) break
                    pageNo++
                }
                nextByPlatform[p] = cur
            } catch (e: Throwable) {
                val failure = e.toApiFailure()
                if (!isAll) throw failure
                failures[p] = failure
                nextByPlatform[p] = null
            }
        }

        // 混排与续页游标全部由 Rust 端（`feedhub-core::appapi::routes`）处理
        val slicesByPage = wanted.map { pageNo ->
            val pageSliceMap = slices[pageNo].orEmpty()
            val sliceInputs = pageSliceMap.map { (p, slice) ->
                RustAppApi.PlatformSliceInput(
                    platform = p.key,
                    items = slice.items.map { item ->
                        RustAppApi.MergedItemInput(
                            itemKey = item.key,
                            itemJson = FeedJson.encodeToString(FeedItem.serializer(), item),
                        )
                    },
                    offset = slice.offset,
                )
            }
            RustAppApi.PageSlicesInput(page = pageNo, slices = sliceInputs)
        }

        val nextByPlat = nextByPlatform.map { (p, c) -> RustAppApi.PlatformCursor(p.key, c?.value) }
        val pStarts = pageStarts.map { (pageNo, map) ->
            RustAppApi.PageStartCursors(
                page = pageNo,
                cursors = map.map { (p, c) -> RustAppApi.PlatformCursor(p.key, c?.value) },
            )
        }

        val mergeOutput = RustAppApi.mergeSearchResults(
            order = order.map { it.key },
            slicesByPage = slicesByPage,
            limit = limit,
            nextByPlatform = nextByPlat,
            pageStarts = pStarts,
            firstPage = wanted.first(),
            failedPlatforms = failures.keys.map { it.key },
        )

        if (failures.isNotEmpty() && failures.size == candidates.size) {
            // 全都失败了：整体报错（错误码取第一家的），正文里仍然给逐家的 error 行
            val first = failures.values.first()
            return ApiReply(first.error.httpStatus, inlineErrors(failures, call), first.error.code)
        }
        val sb = StringBuilder()
        for (e in mergeOutput.shown) sb.append(jsonLine("item", FeedJson.parseToJsonElement(e.itemJson))).append('\n')
        sb.append(inlineErrors(failures, call))
        sb.append(
            jsonLine(
                "done",
                extra = buildMap {
                    put("count", JsonPrimitive(mergeOutput.shown.size))
                    put("hidden", JsonPrimitive(hidden))
                    put("answered", JsonPrimitive(answered))
                    put("platforms", jsonArrayOfStrings(order.map { it.key }))
                    put("page", JsonPrimitive(deepest))
                    put("requests", JsonPrimitive(call.requests))
                    put("truncated", JsonPrimitive(mergeOutput.truncated))
                    put("next", FeedJson.parseToJsonElement(mergeOutput.nextJson))
                    if (failures.isNotEmpty()) {
                        put(
                            "failed",
                            buildJsonObject { failures.forEach { (p, e) -> put(p.key, JsonPrimitive(e.error.code)) } },
                        )
                    }
                },
            ),
        ).append('\n')
        return ok(sb.toString())
    }

    /** 某一家在某一页上的那一段：已经过滤 + 配额；[offset] = 这段前面跳过了几条（续页游标带回来的）。 */
    private class PageSlice(val items: List<FeedItem>, val offset: Int)

    /** 逐家的 `type=error` 行（由 Rust 端组装，保留 retryable 与风控 code 等 extra 字段）。 */
    private fun inlineErrors(failures: Map<Platform, ApiFailure>, call: RequestScope): String {
        if (failures.isEmpty()) return ""
        val items = failures.values.map { failure ->
            val extraJson = if (failure.extra.isEmpty()) {
                null
            } else {
                FeedJson.encodeToString(JsonObject.serializer(), JsonObject(failure.extra))
            }
            RustAppApi.PlatformErrorItem(
                platform = failure.platform?.key,
                code = failure.error.code,
                message = failure.message.orEmpty(),
                retryable = failure.retryable,
                extraJson = extraJson,
            )
        }
        return RustAppApi.renderInlineErrors(items, call.requests)
    }

    // ---------------------------------------------------------------- 详情 / 评论

    /** `/detail`：一条内容的详情（18+ 整条隐藏，只回一行 `type=hidden`，和 cli 一样）。 */
    private suspend fun detail(g: AppGraph, request: ApiRequest, call: RequestScope): ApiReply {
        val p = platformOf(g, request.required("platform"))
        val id = request.required("id")
        g.proxyBlockedReason(p)?.let { throw ApiFailure(ApiError.PROXY_UNAVAILABLE, it, p) }
        val source = g.sourceOrNull(p) as? NativeDetailSource
            ?: throw ApiFailure(ApiError.NOT_SUPPORTED, "${p.label}还没实现原生详情和评论", p)
        val d = source.detail(id)
        if (AdultFilter.hidden(d.item)) {
            return ok(
                jsonLine(
                    "hidden",
                    extra = mapOf(
                        "reason" to JsonPrimitive("adult"),
                        "platform" to JsonPrimitive(p.key),
                        "id" to JsonPrimitive(id),
                        "message" to JsonPrimitive(AdultFilter.NOTICE),
                        "requests" to JsonPrimitive(call.requests),
                    ),
                ) + "\n",
            )
        }
        // 图片清单：`img` 块带 idx / isCover / segments，地址不输出；顺手记进备忘录给 /image 用
        val (images, detailElement) = rewriteDetail(d, g.settings.value.originalImages)
        imageIndex.putDetail(p, id, images)
        val sb = StringBuilder()
        sb.append(jsonLine("detail", detailElement)).append('\n')
        sb.append(
            jsonLine(
                "done",
                extra = mapOf(
                    "count" to JsonPrimitive(1),
                    "requests" to JsonPrimitive(call.requests),
                ),
            ),
        ).append('\n')
        return ok(sb.toString())
    }

    // ---------------------------------------------------------------- 看图

    /**
     * `/image`：**由 App 去取一张图**，回的就是图片本身（JPEG）。
     *
     * - 参数：`platform` + `id`（= `/detail` 的 `data.detail.id`）+ `idx`（`img` 块的序号），可选 `maxw` / `seg` / `comment`；
     * - 图走 App 自己的图片客户端（同一套 UA / 按域名的代理 / fail-closed），**agent 永远不碰图片地址**；
     * - 重复取不再发请求：复用 App 的图片磁盘缓存（Coil 的那个，App 刷流时看过也算），命中时 `X-Feedhub-Requests: 0`；
     * - 接口自己不往磁盘写任何东西（也不给 base64 / URL）；
     * - 长图（高 > 宽 × 3）分段：不传 `seg` 时给第 0 段，总段数在 `X-Feedhub-Segments` 里。
     *
     * 失败照旧是一行 `type=error` + 非 2xx（`not_found` = idx 越界 / 没有这条 / 这条没图）。
     */
    private suspend fun image(g: AppGraph, request: ApiRequest, call: RequestScope): ApiReply {
        val p = platformOf(g, request.required("platform"))
        val id = request.required("id")
        val imgParams = try {
            RustAppApi.validateImageParams(
                idx = request.param("idx"),
                maxw = request.param("maxw"),
                seg = request.param("seg"),
            )
        } catch (e: RustAppApi.ProtocolException) {
            throw ApiFailure(ApiError.BAD_INPUT, e.detail)
        }
        val idx = imgParams.idx
        val maxw = imgParams.maxw
        val seg = imgParams.seg
        val commentId = request.param("comment")
        g.proxyBlockedReason(p)?.let { throw ApiFailure(ApiError.PROXY_UNAVAILABLE, it, p) }
        val source = g.sourceOrNull(p) as? NativeDetailSource
            ?: throw ApiFailure(ApiError.NOT_SUPPORTED, "${p.label}还没实现原生详情和评论", p)
        val images = if (commentId != null) {
            commentImagesFor(g, p, source, id, commentId)
        } else {
            detailImageListFor(g, p, source, id)
        }
        val subject = if (commentId != null) "这条评论" else "这条内容"
        val entry = images.at(idx) ?: throw ApiFailure(
            ApiError.NOT_FOUND,
            when {
                images.count == 0 -> "${subject}没有图片"
                else -> "${subject}只有 ${images.count} 张图（idx 0..${images.count - 1}），没有第 $idx 张"
            },
            p,
        )
        val rendered = imageService.render(entry.url, maxw, seg, call)
        val headers = RustAppApi.imageReplyHeaders(
            idx = idx,
            width = rendered.width,
            height = rendered.height,
            fullWidth = rendered.fullWidth,
            fullHeight = rendered.fullHeight,
            segments = rendered.segments,
            segment = rendered.segment,
            imageCount = images.count,
            requests = call.requests,
        )
        return ApiReply(
            status = 200,
            body = rendered.bytes,
            contentType = "image/jpeg",
            extraHeaders = headers,
        )
    }

    /** `/image` 的 `(platform, id)` → 这条内容的图片清单：先用备忘录，没有再取一次详情。 */
    private suspend fun detailImageListFor(
        g: AppGraph,
        p: Platform,
        source: NativeDetailSource,
        id: String,
    ): ContentImages {
        imageIndex.detail(p, id)?.let { return it.images }
        val d = source.detail(id)
        if (AdultFilter.hidden(d.item)) {
            throw ApiFailure(ApiError.NOT_FOUND, "这条内容是 18+，接口不提供它的图片", p)
        }
        val images = detailImages(d, g.settings.value.originalImages)
        imageIndex.putDetail(p, id, images)
        return images.images
    }

    /**
     * `/image?comment=` 的评论图片清单：先看备忘录（`/comments` / `/replies` 取过的都记着），
     * 没有就自己翻几页找（最多 [MAX_COMMENT_SEARCH_PAGES] 页），找不到就 `not_found`。
     */
    private suspend fun commentImagesFor(
        g: AppGraph,
        p: Platform,
        source: NativeDetailSource,
        id: String,
        commentId: String,
    ): ContentImages {
        imageIndex.comment(p, id, commentId)?.let { return it }
        var cursor: Cursor? = null
        var pages = 0
        while (pages < MAX_COMMENT_SEARCH_PAGES) {
            val page: Page<Comment> = source.comments(id, cursor, CommentSort.DEFAULT)
            pages++
            val found = rememberCommentImages(g, p, id, page.items)
            found[commentId]?.let { images ->
                if (images.count == 0) throw ApiFailure(ApiError.NOT_FOUND, "这条评论没有图片", p)
                return images
            }
            cursor = page.next ?: break
        }
        throw ApiFailure(
            ApiError.NOT_FOUND,
            "没找到评论「${commentId.take(20)}」的图：先用 /comments（或 /replies）把这条评论取出来，" +
                "再带 comment=<评论 id> 来取图",
            p,
        )
    }

    /**
     * 把这一页评论（含楼中楼预览）的图片清单记下来（`/comments` / `/replies` / `/image` 共用）。
     * 返回这一页里**每条评论**的清单（没有图的也在里面，方便判断"这条评论没图"还是"根本没有这条评论"）。
     */
    private fun rememberCommentImages(
        g: AppGraph,
        p: Platform,
        id: String,
        comments: List<Comment>,
    ): Map<String, ContentImages> {
        val originalImages = g.settings.value.originalImages
        val json = FeedJson.encodeToString(kotlinx.serialization.builtins.ListSerializer(Comment.serializer()), comments)
        val extracted = RustAppApi.extractCommentsImages(json, originalImages)
        val out = LinkedHashMap<String, ContentImages>()
        for (pair in extracted) {
            val imgs = pair.images.toModel()
            out[pair.commentId] = imgs
            imageIndex.putComment(p, id, pair.commentId, imgs)
        }
        return out
    }

    /**
     * `/comments` 和 `/replies`（[replies] = true 时是楼中楼）：翻页、按 id 去重、`limit` 截断。
     *
     * `limit` 停在某一页中间时 `done.truncated=true`、`done.next` 是**续页游标**（[ResumeCursor]：
     * 这一页的起始平台游标 + 这一页已经给到第几条）。下一次带回来就重新取那一页、跳过已经给过的，
     * 接着往下给；给完这一页再往后就是平台自己的游标。第一页（游标是 null）一样能续。
     */
    private suspend fun comments(g: AppGraph, request: ApiRequest, call: RequestScope, replies: Boolean): ApiReply {
        val p = platformOf(g, request.required("platform"))
        val id = request.required("id")
        val commentsParams = try {
            RustAppApi.validateCommentsParams(
                replies = replies,
                comment = request.param("comment"),
                commentId = request.param("commentId"),
                pages = request.param("pages"),
                sort = request.param("sort"),
                limit = request.param("limit"),
            )
        } catch (e: RustAppApi.ProtocolException) {
            throw ApiFailure(ApiError.BAD_INPUT, e.detail)
        }
        val commentId = commentsParams.commentId
        val pages = commentsParams.pages
        val sort = if (!replies) enumOf<CommentSort>(commentsParams.sort, "sort") else CommentSort.DEFAULT
        val limit = commentsParams.limit
        val start = ResumeCursor.decode(request.param("cursor"))
        g.proxyBlockedReason(p)?.let { throw ApiFailure(ApiError.PROXY_UNAVAILABLE, it, p) }
        val source = g.sourceOrNull(p) as? NativeDetailSource
            ?: throw ApiFailure(ApiError.NOT_SUPPORTED, "${p.label}还没实现原生详情和评论", p)

        val sb = StringBuilder()
        var n = 0
        var truncated = false
        var nextOut: String? = null
        val seen = HashSet<String>()
        var pageCursor: Cursor? = start.platform
        for (i in 0 until pages) {
            val pageStart = pageCursor
            val page: Page<Comment> =
                if (replies) source.replies(id, commentId!!, pageStart) else source.comments(id, pageStart, sort)
            // 评论里的图也记一份：agent 之后带 comment=<评论 id> 来 /image 取图就不用重翻这一页
            rememberCommentImages(g, p, id, page.items)
            // 续页游标里的"跳过几条"只作用在带它回来的那一页（= 本次的第一页）
            var skip = if (i == 0) start.skip else 0
            var consumed = 0
            var fresh = 0
            var duplicates = 0
            for (c in page.items) {
                if (skip > 0) {
                    // 这一页里已经给过的那几条：跳过（也算"这一页走到第几条"）
                    skip--
                    consumed++
                    continue
                }
                if (!seen.add(c.id)) {
                    duplicates++
                    consumed++
                    continue
                }
                if (limit != null && n >= limit) {
                    truncated = true
                    break
                }
                fresh++
                n++
                consumed++
                sb.append(jsonLine("comment", commentJson(c))).append('\n')
            }
            if (truncated) {
                // 停在这一页的第 consumed 条之前：把"这一页的起始游标 + 已经给了几条"包成续页游标
                nextOut = ResumeCursor.encode(pageStart, consumed)
                break
            }
            pageCursor = page.next
            if (pageCursor == null) {
                nextOut = null
                break
            }
            nextOut = pageCursor.value
            // 整页都是重复的：再翻也只会重复，当到底了（和 App 的 PagedState 一致）
            if (fresh == 0 && duplicates > 0) break
        }
        sb.append(
            jsonLine(
                "done",
                extra = buildMap {
                    put("count", JsonPrimitive(n))
                    put("next", nextOut?.let { JsonPrimitive(it) } ?: JsonNull)
                    put("truncated", JsonPrimitive(truncated))
                    put("requests", JsonPrimitive(call.requests))
                },
            ),
        ).append('\n')
        return ok(sb.toString())
    }

    // ---------------------------------------------------------------- 本地收藏 / 历史

    /**
     * `/library/favorites`：**读 App 自己的资料库**（[AppGraph.library]），一个网络请求都不发。
     *
     * `since` 只能在内存里过滤（App 的 `FavoriteQuery` 没有这个条件），所以带 `since` 时先多取一些
     * （最多 [MAX_LIBRARY_LIMIT] 条）再筛、再按 `limit` 截——极端情况下比 `limit` 条略旧的数据会被筛掉，
     * 文档里写明了。
     */
    private suspend fun favorites(g: AppGraph, request: ApiRequest, call: RequestScope): ApiReply {
        val platform = request.param("platform")?.let { platformOf(g, it) }
        val since = request.param("since")?.let { parseSince(it) }
        val query = (request.param("q") ?: request.param("query")).orEmpty()
        val limit = libraryLimit(request)
        val rows = g.library.favorites(
            FavoriteQuery(platform = platform, search = query, limit = if (since != null) MAX_LIBRARY_LIMIT else limit),
        )
            .filter { since == null || it.addedAt >= since }
            .take(limit)
        val sb = StringBuilder()
        for (row in rows) {
            sb.append(
                jsonLine(
                    "favorite",
                    itemJson(row.item),
                    buildMap {
                        put("addedAt", JsonPrimitive(row.addedAt))
                        row.folder?.let { put("folder", JsonPrimitive(it)) }
                    },
                ),
            ).append('\n')
        }
        sb.append(
            jsonLine(
                "done",
                extra = mapOf(
                    "count" to JsonPrimitive(rows.size),
                    "command" to JsonPrimitive("library favorites"),
                    "requests" to JsonPrimitive(call.requests),
                ),
            ),
        ).append('\n')
        return ok(sb.toString())
    }

    /** `/library/history`：同上（`failedOnly=1` 只看没看完的：有进度且进度 < 时长）。 */
    private suspend fun history(g: AppGraph, request: ApiRequest, call: RequestScope): ApiReply {
        val platform = request.param("platform")?.let { platformOf(g, it) }
        val since = request.param("since")?.let { parseSince(it) }
        val query = (request.param("q") ?: request.param("query")).orEmpty()
        val failedOnly = request.param("failedOnly")?.let { it == "1" || it.equals("true", true) } ?: false
        val limit = libraryLimit(request)
        val rows = g.library.history(
            HistoryQuery(platform = platform, search = query, limit = if (since != null) MAX_LIBRARY_LIMIT else limit),
        )
            .filter { since == null || it.visitedAt >= since }
            .filter {
                if (!failedOnly) return@filter true
                val progress = it.progressMs ?: 0
                val duration = it.durationMs ?: 0
                progress > 0 && duration > 0 && progress < duration
            }
            .take(limit)
        val sb = StringBuilder()
        for (row in rows) {
            sb.append(
                jsonLine(
                    "history",
                    itemJson(row.item),
                    buildMap {
                        put("visitedAt", JsonPrimitive(row.visitedAt))
                        row.progressMs?.let { put("progressMs", JsonPrimitive(it)) }
                        row.durationMs?.let { put("durationMs", JsonPrimitive(it)) }
                    },
                ),
            ).append('\n')
        }
        sb.append(
            jsonLine(
                "done",
                extra = mapOf(
                    "count" to JsonPrimitive(rows.size),
                    "command" to JsonPrimitive("library history"),
                    "requests" to JsonPrimitive(call.requests),
                ),
            ),
        ).append('\n')
        return ok(sb.toString())
    }

    // ---------------------------------------------------------------- 小工具

    private fun ok(body: String) = ApiReply(200, body)

    /**
     * `/search`、`/comments`、`/replies` 的 `limit`：**一次调用最多给一页**，所以和资料库一样设上限
     * （`> 500` 直接 `bad_input`，别让 agent 以为一条命令能要一千条）。
     */
    private fun pageLimit(request: ApiRequest): Int? {
        try {
            return RustAppApi.validatePageLimit(request.param("limit"))
        } catch (e: RustAppApi.ProtocolException) {
            throw ApiFailure(ApiError.BAD_INPUT, e.detail)
        }
    }

    private fun libraryLimit(request: ApiRequest): Int {
        try {
            return RustAppApi.validateLibraryLimit(request.param("limit"))
        } catch (e: RustAppApi.ProtocolException) {
            throw ApiFailure(ApiError.BAD_INPUT, e.detail)
        }
    }

    private fun intParam(request: ApiRequest, name: String): Int? {
        try {
            return RustAppApi.validateIntParam(name, request.param(name))
        } catch (e: RustAppApi.ProtocolException) {
            throw ApiFailure(ApiError.BAD_INPUT, e.detail)
        }
    }

    /** 平台参数：id（`bili`）和 JSON 里的存储名（`BILIBILI`）都认，和 cli 一样。 */
    private fun platformOf(g: AppGraph, key: String): Platform =
        g.platforms.find(key)
            ?: runCatching { Platform.fromStored(key) }.getOrNull()?.let { g.platforms.find(it.key) }
            ?: throw ApiFailure(ApiError.BAD_INPUT, "未知平台 $key，可选：${g.platforms.keysText(", ")}")

    private inline fun <reified T : Enum<T>> enumOf(text: String, what: String): T =
        enumValues<T>().firstOrNull { it.name.equals(text, ignoreCase = true) }
            ?: throw ApiFailure(
                ApiError.BAD_INPUT,
                "未知的 $what「$text」，可选：${enumValues<T>().joinToString { it.name.lowercase() }}",
            )

    companion object {
        const val DEFAULT_LIBRARY_LIMIT = 20
        const val MAX_LIBRARY_LIMIT = 500

        /** `/search`、`/comments`、`/replies` 的 `limit` 上限（一次调用最多给一页）。 */
        const val MAX_PAGE_LIMIT = 500

        /** `/image?comment=` 自己找评论时最多翻几页（找不到就 `not_found`，让 agent 先 /comments）。 */
        const val MAX_COMMENT_SEARCH_PAGES = 3
    }
}

/**
 * **续页游标**：逻辑完全下沉至 Rust 端（`feedhub-core::appapi::cursor`）。
 */
internal object ResumeCursor {
    class Decoded(val platform: Cursor?, val skip: Int)

    fun decode(raw: String?): Decoded {
        val d = RustAppApi.decodeResumeCursor(raw)
        return Decoded(d.platform?.let { Cursor(it) }, d.skip)
    }

    fun encode(platform: Cursor?, skip: Int): String {
        return RustAppApi.encodeResumeCursor(platform?.value, skip)
    }
}

/**
 * `since` 的解析：下沉至 Rust 端（`feedhub-core::appapi::since`）。
 */
internal fun parseSince(text: String, now: Long = System.currentTimeMillis()): Long {
    try {
        return RustAppApi.parseSince(text, now)
    } catch (e: RustAppApi.ProtocolException) {
        throw ApiFailure(ApiError.BAD_INPUT, e.detail)
    }
}
