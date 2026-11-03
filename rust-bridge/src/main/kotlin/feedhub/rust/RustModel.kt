package feedhub.rust

import feedhub.core.ApiException
import feedhub.core.AudioStream
import feedhub.core.Block
import feedhub.core.Comment
import feedhub.core.Cursor
import feedhub.core.Danmaku
import feedhub.core.DanmakuMode
import feedhub.core.DetailTarget
import feedhub.core.FeedDetail
import feedhub.core.FeedItem
import feedhub.core.FollowKind
import feedhub.core.FollowState
import feedhub.core.FollowTarget
import feedhub.core.ItemKind
import feedhub.core.Media
import feedhub.core.NotLoggedInException
import feedhub.core.Page
import feedhub.core.QrLogin
import feedhub.core.QrLoginState
import feedhub.core.ParseException
import feedhub.core.PlayInfo
import feedhub.core.Platform
import feedhub.core.ProgressiveStream
import feedhub.core.Quality
import feedhub.core.RichText
import feedhub.core.RiskControlException
import feedhub.core.SegmentBase
import feedhub.core.SignatureException
import feedhub.core.SourceException
import feedhub.core.Span
import feedhub.core.Stats
import feedhub.core.TimePrecision
import feedhub.core.UserRef
import feedhub.core.VideoCodec
import feedhub.core.VideoInfo
import feedhub.core.VideoPart
import feedhub.core.VideoStream
import uniffi.feedhub_ffi.FfiComment
import uniffi.feedhub_ffi.FfiCommentPage
import uniffi.feedhub_ffi.FfiDanmaku
import uniffi.feedhub_ffi.FfiDanmakuMode
import uniffi.feedhub_ffi.FfiDetail
import uniffi.feedhub_ffi.FfiDetailBlock
import uniffi.feedhub_ffi.FfiDetailBody
import uniffi.feedhub_ffi.FfiDetailFollow
import uniffi.feedhub_ffi.FfiDetailSpan
import uniffi.feedhub_ffi.FfiDetailTarget
import uniffi.feedhub_ffi.FfiException
import uniffi.feedhub_ffi.FfiFollowKind
import uniffi.feedhub_ffi.FfiFollowTarget
import uniffi.feedhub_ffi.FfiItem
import uniffi.feedhub_ffi.FfiItemKind
import uniffi.feedhub_ffi.FfiMedia
import uniffi.feedhub_ffi.FfiPage
import uniffi.feedhub_ffi.FfiPlayCodec
import uniffi.feedhub_ffi.FfiPlayInfo
import uniffi.feedhub_ffi.FfiPlaySegmentBase
import uniffi.feedhub_ffi.FfiTimePrecision

/**
 * Rust 适配层的公用零件（**只给 `feedhub.rust` 里的适配对象用，不对外**）。
 *
 * 为什么单独一个文件：每个平台的适配对象（`RustBili` / `RustTieba` / …）是并行开发的，
 * 大家都要"FfiException → 现有异常"和"FfiItem → FeedItem"这两段，抄一份迟早会漂；
 * 放这里，各平台只管自己的几个函数。
 *
 * 四份零件现在都在这里，`Rust*` 门面一个字节的映射逻辑都不再各写一份：
 * - [guard] / [toSourceException]：**纯函数**入口的错误出口（`Network` / `ProxyUnavailable` 折成
 *   `SourceException`——纯函数不发请求，碰不到；平台对象那条路走 [HostedCall.guard]）；
 * - [toPage] / [buildItem]：`FfiPage` / `FfiItem` → `Page<FeedItem>`（转发是"节点数组 + 下标"）；
 * - [toItemKind] / [toTimePrecision] / [toMedia]：三个枚举 / 媒体类型的映射；
 * - [toDetail] / [toCommentPage]和 [toPlayInfo] / [toDanmaku]：详情 / 评论 /
 *   播放 / 弹幕的共用映射，11 家直接用。
 */
internal object RustModel {
    /**
     * 调 Rust：加载 `.so`、把 `FfiException` 翻回现有异常。
     * 加载失败**不悄悄退回 Kotlin 实现**，而是给一句说得清的话（和 `RustBili` 的行为一致）。
     */
    inline fun <T> guard(platform: Platform, block: () -> T): T {
        try {
            RustNative.ensureLoaded()
            return block()
        } catch (e: FfiException) {
            throw e.toSourceException(platform)
        } catch (e: SourceException) {
            throw e
        } catch (e: UnsatisfiedLinkError) {
            throw SourceException(platform, "Rust 核心加载失败（找不到 libfeedhub_ffi.so）：${e.message}")
        } catch (e: IllegalStateException) {
            throw SourceException(platform, "Rust 核心不可用：${e.message}")
        }
    }

    fun FfiException.toSourceException(platform: Platform): SourceException = when (this) {
        is FfiException.NotLoggedIn -> NotLoggedInException(platform, detail ?: "未登录或登录已失效")
        is FfiException.RiskControl -> RiskControlException(platform, code, detail)
        is FfiException.Signature -> SignatureException(platform, detail)
        is FfiException.Api -> ApiException(platform, code, detail)
        is FfiException.Parse -> ParseException(platform, detail)
        is FfiException.Internal -> SourceException(platform, "Rust 内部错误：$detail")
        // 这三个只会从宿主回调（平台对象，见 RustBili）回来；纯函数入口不发请求，碰不到。
        // `CaptchaRequired` 也一样（酷安评论那条路才有），纯函数入口永远走不到这里。
        is FfiException.Network, is FfiException.ProxyUnavailable ->
            SourceException(platform, "Rust 纯函数入口不该出现宿主网络错误：$message")
        is FfiException.CaptchaRequired ->
            CaptchaRequiredException(platform, captchaId, field, detail)
    }

    fun toPage(page: FfiPage, platform: Platform): Page<FeedItem> {
        val nodes = page.nodes
        val items = page.roots.map { index -> buildItem(index, nodes, platform) }
        return Page(items, page.next?.let { Cursor(it) })
    }

    /** `FfiItem` → `FeedItem`。转发是"节点数组 + 下标"（UniFFI 不支持递归 record），带深度上限。 */
    fun buildItem(index: UInt, nodes: List<FfiItem>, platform: Platform, depth: Int = 0): FeedItem {
        check(depth < 64) { "转发的原内容嵌套太深（$depth 层）：Rust 侧的下标可能有环" }
        val f = nodes.getOrNull(index.toInt()) ?: error("Rust 返回的条目下标越界：$index / ${nodes.size}")
        return FeedItem(
            platform = platform,
            id = f.id,
            kind = f.kind.toItemKind(),
            title = f.title,
            summary = f.summary,
            language = f.language,
            author = f.author?.let {
                UserRef(
                    platform = platform,
                    id = it.id,
                    name = it.name,
                    avatar = it.avatar,
                    url = it.url,
                )
            },
            publishedAt = f.publishedAt,
            publishedAtPrecision = f.publishedAtPrecision.toTimePrecision(),
            media = f.media.map { it.toMedia() },
            stats = Stats(
                views = f.stats.views,
                likes = f.stats.likes,
                comments = f.stats.comments,
                shares = f.stats.shares,
                danmaku = f.stats.danmaku,
                favorites = f.stats.favorites,
                coins = f.stats.coins,
            ),
            reason = f.reason,
            community = f.community,
            communityIcon = f.communityIcon,
            repost = f.repost?.let { buildItem(it, nodes, platform, depth + 1) },
            emotes = LinkedHashMap<String, String>(f.emotes.size).apply {
                for (e in f.emotes) put(e.code, e.url)
            },
            adult = f.adult,
            url = f.url,
            detail = when (val d = f.detail) {
                is FfiDetailTarget.Native -> DetailTarget.Native(d.id, d.platform?.let { Platform(it) })
                is FfiDetailTarget.Web -> DetailTarget.Web(d.url)
            },
        )
    }

    fun FfiItemKind.toItemKind(): ItemKind = when (this) {
        FfiItemKind.VIDEO -> ItemKind.VIDEO
        FfiItemKind.ARTICLE -> ItemKind.ARTICLE
        FfiItemKind.ANSWER -> ItemKind.ANSWER
        FfiItemKind.QUESTION -> ItemKind.QUESTION
        FfiItemKind.POST -> ItemKind.POST
        FfiItemKind.MOMENT -> ItemKind.MOMENT
        FfiItemKind.LIVE -> ItemKind.LIVE
        FfiItemKind.OTHER -> ItemKind.OTHER
    }

    fun FfiTimePrecision.toTimePrecision(): TimePrecision = when (this) {
        FfiTimePrecision.SECOND -> TimePrecision.SECOND
        FfiTimePrecision.MINUTE -> TimePrecision.MINUTE
        FfiTimePrecision.HOUR -> TimePrecision.HOUR
        FfiTimePrecision.DAY -> TimePrecision.DAY
        FfiTimePrecision.MONTH -> TimePrecision.MONTH
        FfiTimePrecision.YEAR -> TimePrecision.YEAR
    }

    fun FfiMedia.toMedia(): Media = when (this) {
        is FfiMedia.Image -> Media.Image(url, width, height)
        is FfiMedia.Video -> Media.Video(cover, durationSec, width, height)
    }

    // ---------------------------------------------------------------- 详情 / 评论
    //
    // 和 [toPage] 并列，11 家共用。
    // 两处"节点数组 + 下标"：详情条目 / 相关推荐在 [FfiDetail.nodes] 里，楼中楼预览在
    // [FfiCommentPage.nodes] 里（UniFFI 0.29 不支持递归 record）。

    /** 详情。`item` 必须存在（拿不到条目就是 Rust 那边的解析 bug，直接报出来）。 */
    fun toDetail(detail: FfiDetail, platform: Platform): FeedDetail {
        val nodes = detail.nodes
        val index = detail.item ?: error("Rust 返回的详情没有条目下标（nodes=${nodes.size}）")
        return FeedDetail(
            item = buildItem(index, nodes, platform),
            body = toRichText(detail.body),
            video = detail.video?.let { v ->
                VideoInfo(
                    parts = v.parts.map { VideoPart(it.id, it.title, it.durationSec, it.width, it.height) },
                    danmakuSegmentMs = v.danmakuSegmentMs,
                )
            },
            follow = detail.follow?.let { toFollowState(it) },
            related = detail.related.map { buildItem(it, nodes, platform) },
        )
    }

    /** 一页评论（一级评论 + 楼中楼预览）。 */
    fun toCommentPage(page: FfiCommentPage, platform: Platform): Page<Comment> {
        val nodes = page.nodes
        return Page(page.roots.map { toComment(it, nodes, platform) }, page.next?.let { Cursor(it) })
    }

    /** `FfiComment` → `Comment`。楼中楼预览也是"节点数组 + 下标"，带深度上限防环。 */
    fun toComment(index: UInt, nodes: List<FfiComment>, platform: Platform, depth: Int = 0): Comment {
        check(depth < 64) { "楼中楼嵌套太深（$depth 层）：Rust 侧的下标可能有环" }
        val c = nodes.getOrNull(index.toInt()) ?: error("Rust 返回的评论下标越界：$index / ${nodes.size}")
        return Comment(
            platform = platform,
            id = c.id,
            author = c.author?.toUserRef(),
            content = toRichText(c.content),
            publishedAt = c.publishedAt,
            publishedAtPrecision = c.publishedAtPrecision.toTimePrecision(),
            likes = c.likes,
            replyCount = c.replyCount,
            replyTo = c.replyTo?.toUserRef(),
            ipLocation = c.ipLocation,
            floor = c.floor,
            pinned = c.pinned,
            previewReplies = c.previewReplies.map { toComment(it, nodes, platform, depth + 1) },
            language = c.language,
        )
    }

    fun toRichText(body: FfiDetailBody): RichText = RichText(body.blocks.map { it.toBlock() })

    fun FfiDetailBlock.toBlock(): Block = when (this) {
        is FfiDetailBlock.Paragraph -> Block.Paragraph(spans.map { it.toSpan() })
        is FfiDetailBlock.Heading -> Block.Heading(spans.map { it.toSpan() })
        is FfiDetailBlock.Quote -> Block.Quote(spans.map { it.toSpan() })
        is FfiDetailBlock.Code -> Block.Code(text, language)
        is FfiDetailBlock.Image -> Block.Image(url, width, height, original, caption)
        is FfiDetailBlock.Card -> Block.Card(title, subtitle, cover, url, target?.toDetailTarget())
        is FfiDetailBlock.Video -> Block.Video(
            id = id,
            partId = partId,
            cover = cover,
            durationSec = durationSec,
            width = width,
            height = height,
            title = title,
            url = url,
        )
        is FfiDetailBlock.Divider -> Block.Divider
    }

    fun FfiDetailSpan.toSpan(): Span = when (this) {
        is FfiDetailSpan.Text -> Span.Text(text, bold, italic)
        is FfiDetailSpan.Link -> Span.Link(text, url, target?.toDetailTarget())
        is FfiDetailSpan.Mention -> Span.Mention(text, user.toUserRef())
        is FfiDetailSpan.Emote -> Span.Emote(text, url)
    }

    /** `FfiUserRef` → `UserRef`（平台 id 用 Rust 带回来的那个字符串）。 */
    fun uniffi.feedhub_ffi.FfiUserRef.toUserRef(): UserRef = UserRef(Platform(platform), id, name, avatar, url)

    /**
     * `FfiUserRef?` → `UserRef?`：`whoami` 这类"没登录就是 null"的返回值用。
     * 平台对象把它作为 `native.whoami()?.toUserRef()` 的出口（`RustModel.run { … }` 那层不用写在门面里）。
     */
    fun userRef(user: uniffi.feedhub_ffi.FfiUserRef?): UserRef? = user?.toUserRef()

    fun FfiDetailTarget.toDetailTarget(): DetailTarget = when (this) {
        is FfiDetailTarget.Native -> DetailTarget.Native(id, platform?.let { Platform(it) })
        is FfiDetailTarget.Web -> DetailTarget.Web(url)
    }

    fun toFollowState(follow: FfiDetailFollow): FollowState = FollowState(toFollowTarget(follow.target), follow.following)

    fun toFollowTarget(target: FfiFollowTarget): FollowTarget = FollowTarget(
        platform = Platform(target.platform),
        kind = when (target.kind) {
            FfiFollowKind.USER -> FollowKind.USER
            FfiFollowKind.FORUM -> FollowKind.FORUM
        },
        id = target.id,
        name = target.name,
        avatar = target.avatar,
        description = target.description,
        url = target.url,
    )

    // ---------------------------------------------------------------- 播放 / 弹幕
    //
    // 每家平台都有播放地址（B站 是第一个搬进 Rust 的），映射只写这一份——下一家别抄。
    // 三种"拿不到"的表示都在这里落成 Kotlin 的空值：`None` → null、空列表 → emptyList、没有 codec → null。

    /** 播放地址。`headers` 是**有序**的（`LinkedHashMap`：`Referer` 在 `User-Agent` 前面）。 */
    fun toPlayInfo(play: FfiPlayInfo): PlayInfo = PlayInfo(
        durationMs = play.durationMs,
        quality = play.quality,
        qualities = play.qualities.map { Quality(it.id, it.label, it.available) },
        video = play.video.map { s ->
            VideoStream(
                quality = s.quality,
                codec = s.codec.toVideoCodec(),
                codecs = s.codecs,
                url = s.url,
                backupUrls = s.backupUrls,
                bandwidth = s.bandwidth,
                width = s.width,
                height = s.height,
                frameRate = s.frameRate,
                segmentBase = s.segmentBase?.toSegmentBase(),
            )
        },
        audio = play.audio.map { s ->
            AudioStream(
                id = s.id,
                codecs = s.codecs,
                url = s.url,
                backupUrls = s.backupUrls,
                bandwidth = s.bandwidth,
                segmentBase = s.segmentBase?.toSegmentBase(),
            )
        },
        progressive = play.progressive.map { s ->
            ProgressiveStream(
                quality = s.quality,
                url = s.url,
                backupUrls = s.backupUrls,
                sizeBytes = s.sizeBytes,
                width = s.width,
                height = s.height,
                codec = s.codec?.toVideoCodec(),
                bandwidth = s.bandwidth,
            )
        },
        headers = LinkedHashMap<String, String>(play.headers.size).apply {
            for (h in play.headers) put(h.name, h.value)
        },
    )

    fun FfiPlaySegmentBase.toSegmentBase(): SegmentBase = SegmentBase(initialization, indexRange)

    fun FfiPlayCodec.toVideoCodec(): VideoCodec = when (this) {
        FfiPlayCodec.AVC -> VideoCodec.AVC
        FfiPlayCodec.HEVC -> VideoCodec.HEVC
        FfiPlayCodec.AV1 -> VideoCodec.AV1
        FfiPlayCodec.OTHER -> VideoCodec.OTHER
    }

    /** 一条弹幕。 */
    fun toDanmaku(danmaku: FfiDanmaku): Danmaku = Danmaku(
        id = danmaku.id,
        timeMs = danmaku.timeMs,
        mode = danmaku.mode.toDanmakuMode(),
        text = danmaku.text,
        color = danmaku.color,
        fontSize = danmaku.fontSize,
        weight = danmaku.weight,
    )

    /** 一段弹幕（一次调用换一整段，不要一条条过边界）。 */
    fun toDanmakuList(items: List<FfiDanmaku>): List<Danmaku> = items.map(::toDanmaku)

    fun FfiDanmakuMode.toDanmakuMode(): DanmakuMode = when (this) {
        FfiDanmakuMode.SCROLL -> DanmakuMode.SCROLL
        FfiDanmakuMode.TOP -> DanmakuMode.TOP
        FfiDanmakuMode.BOTTOM -> DanmakuMode.BOTTOM
        FfiDanmakuMode.OTHER -> DanmakuMode.OTHER
    }

    // 账号层：扫码登录 / 关注列表

    /** `FfiUserRef` → `UserRef`（给平台门面用；`toUserRef()` 是成员扩展，外面调不方便）。 */
    fun toUser(user: uniffi.feedhub_ffi.FfiUserRef): UserRef = user.toUserRef()

    /** `FfiQrLogin` → `QrLogin`（字段一对一；二维码内容原样交给前端画）。 */
    fun toQrLogin(login: uniffi.feedhub_ffi.FfiQrLogin): QrLogin = QrLogin(
        platform = Platform(login.platform),
        content = login.content,
        token = login.token,
        extra = login.extra.associate { it.name to it.value },
        expiresAtMs = login.expiresAtMs,
        pollIntervalMs = login.pollIntervalMs,
        hint = login.hint,
    )

    /** `FfiQrState` → `QrLoginState`。 */
    fun toQrState(state: uniffi.feedhub_ffi.FfiQrState): QrLoginState = when (state.kind) {
        uniffi.feedhub_ffi.FfiQrStateKind.WAITING -> QrLoginState.Waiting
        uniffi.feedhub_ffi.FfiQrStateKind.SCANNED -> QrLoginState.Scanned
        uniffi.feedhub_ffi.FfiQrStateKind.SUCCESS -> QrLoginState.Success(state.user?.toUserRef())
        uniffi.feedhub_ffi.FfiQrStateKind.EXPIRED -> QrLoginState.Expired
        uniffi.feedhub_ffi.FfiQrStateKind.FAILED -> QrLoginState.Failed(state.message ?: "登录失败")
    }

    /** `FfiAccountFollowPage` → `Page<FollowTarget>`。 */
    fun toFollowPage(page: uniffi.feedhub_ffi.FfiAccountFollowPage): Page<FollowTarget> =
        Page(page.items.map { toFollowTarget(it) }, page.next?.let(::Cursor))
}
