package feedhub.rust

import uniffi.feedhub_ffi.FfiApiErrorCode
import uniffi.feedhub_ffi.FfiApiFailureKind
import uniffi.feedhub_ffi.FfiApiImageIndex
import uniffi.feedhub_ffi.FfiApiProtocolException
import uniffi.feedhub_ffi.FfiContentImages
import uniffi.feedhub_ffi.FfiDateToEpoch
import uniffi.feedhub_ffi.FfiDetailImages
import uniffi.feedhub_ffi.FfiImageSegments
import uniffi.feedhub_ffi.FfiMergedItemInput
import uniffi.feedhub_ffi.FfiPageSlicesInput
import uniffi.feedhub_ffi.FfiPageStartCursors
import uniffi.feedhub_ffi.FfiPlatformCursor
import uniffi.feedhub_ffi.FfiPlatformErrorItem
import uniffi.feedhub_ffi.FfiPlatformSliceInput
import uniffi.feedhub_ffi.apiClassify
import uniffi.feedhub_ffi.apiErrorCodes
import uniffi.feedhub_ffi.apiErrorOfKey
import uniffi.feedhub_ffi.apiExtractCommentsImages
import uniffi.feedhub_ffi.apiImageLimits
import uniffi.feedhub_ffi.apiImageReplyHeaders
import uniffi.feedhub_ffi.apiIsLong
import uniffi.feedhub_ffi.apiMergeSearchResults
import uniffi.feedhub_ffi.apiParseSince
import uniffi.feedhub_ffi.apiRenderCapabilities
import uniffi.feedhub_ffi.apiRenderErrorLine
import uniffi.feedhub_ffi.apiRenderInlineErrors
import uniffi.feedhub_ffi.apiRenderPlatforms
import uniffi.feedhub_ffi.apiResumeCursorDecode
import uniffi.feedhub_ffi.apiResumeCursorEncode
import uniffi.feedhub_ffi.apiRewriteDetailJson
import uniffi.feedhub_ffi.apiScaled
import uniffi.feedhub_ffi.apiSchemaVersion
import uniffi.feedhub_ffi.apiSegmentHeight
import uniffi.feedhub_ffi.apiSegmentTop
import uniffi.feedhub_ffi.apiSegments
import uniffi.feedhub_ffi.apiSegmentsFor
import uniffi.feedhub_ffi.apiValidateCommentsParams
import uniffi.feedhub_ffi.apiValidateImageParams
import uniffi.feedhub_ffi.apiValidateIntParam
import uniffi.feedhub_ffi.apiValidateLibraryLimit
import uniffi.feedhub_ffi.apiValidatePageLimit
import uniffi.feedhub_ffi.apiValidateSearchParams
import feedhub.rust.RustAppApi.FailureKind

/**
 * **桌面对外接口的协议常量 + 取图几何**。
 *
 * 规则在 `rust/crates/feedhub-core/src/appapi.rs`：错误码表（码 / HTTP 状态码 / 能不能重试）、
 * 异常种类 → 错误码的归类、图片缩放与长图分段。
 *
 * **留在宿主**：unix socket 的监听循环 / HTTP 子集在 `UnixHttpServer`，
 * 真正取图（Coil + Skia）在 `ApiImageService`——Rust 只给"缩到多宽、裁哪一段"的几何。
 *
 * **没有开关**：`.so` 加载失败就抛（和 `ProxyRules` / `RustSettings` 一样）。
 */
object RustAppApi {
    class ProtocolException(val code: String, val detail: String) : RuntimeException(detail)

    /** 输出格式的版本（`/capabilities` 的 `schema`）。 */
    val schemaVersion: Int by lazy { native { apiSchemaVersion().toInt() } }

    /** 一次失败的**种类**：Kotlin 只认异常类型，这个决定用哪个错误码。 */
    enum class FailureKind {
        RISK_CONTROL, NOT_LOGGED_IN, NOT_SUPPORTED, CAPTCHA_REQUIRED, PROXY_UNAVAILABLE,
        PLATFORM_ERROR, PARSE, NETWORK, NOT_FOUND, INTERNAL,
        ;

        internal fun toFfi(): FfiApiFailureKind = when (this) {
            RISK_CONTROL -> FfiApiFailureKind.RISK_CONTROL
            NOT_LOGGED_IN -> FfiApiFailureKind.NOT_LOGGED_IN
            NOT_SUPPORTED -> FfiApiFailureKind.NOT_SUPPORTED
            CAPTCHA_REQUIRED -> FfiApiFailureKind.CAPTCHA_REQUIRED
            PROXY_UNAVAILABLE -> FfiApiFailureKind.PROXY_UNAVAILABLE
            PLATFORM_ERROR -> FfiApiFailureKind.PLATFORM_ERROR
            PARSE -> FfiApiFailureKind.PARSE
            NETWORK -> FfiApiFailureKind.NETWORK
            NOT_FOUND -> FfiApiFailureKind.NOT_FOUND
            INTERNAL -> FfiApiFailureKind.INTERNAL
        }
    }

    /** 一个错误码的字段（字符串 + 状态码 + 能不能重试）。 */
    class ErrorCode(val code: String, val httpStatus: Int, val retryable: Boolean)

    /** 全部错误码（**按 Kotlin `ApiError` 的声明顺序**，`/capabilities` 直接列它）。 */
    val errorCodes: List<ErrorCode> by lazy {
        native { apiErrorCodes().map { it.toModel() } }
    }

    /** 按字符串找错误码；不认识就是 null。 */
    fun errorOfKey(key: String): ErrorCode? = native {
        apiErrorOfKey(key)?.toModel()
    }

    /** **`ApiError` enum 名 → 字段**。名字对不上（有人在 Kotlin 加了枚举但没在 Rust 加）直接抛。 */
    fun codeOf(enumName: String): ErrorCode = native {
        val ffi = apiErrorOfKey(enumKey(enumName))
            ?: throw IllegalStateException("错误码 `$enumName` 不在 Rust 的表里")
        ffi.toModel()
    }

    /** 种类 → 错误码（唯一的一份映射）。 */
    fun classify(kind: FailureKind): ErrorCode = native { apiClassify(kind.toFfi()).toModel() }

    // ---------------------------------------------------------------- 图片几何

    /** `maxw` 默认值 / 上限、长图阈值。 */
    class ImageLimits(val defaultMaxWidth: Int, val maxMaxWidth: Int, val longRatio: Int)

    val imageLimits: ImageLimits by lazy {
        native {
            val ffi = apiImageLimits()
            ImageLimits(ffi.defaultMaxWidth, ffi.maxMaxWidth, ffi.longRatio)
        }
    }

    /** 一张图的分段方案。 */
    class Segments(
        val width: Int,
        val height: Int,
        val segmentHeight: Int,
        val overlap: Int,
        val count: Int,
    ) {
        /** 第 [index] 段在缩放后图片里的起始行。 */
        fun top(index: Int): Int = native { apiSegmentTop(toFfi(), index) }

        /** 第 [index] 段有多少行（最后一段可能短）。 */
        fun heightAt(index: Int): Int = native { apiSegmentHeight(toFfi(), index) }

        internal fun toFfi(): FfiImageSegments = FfiImageSegments(
            width = width,
            height = height,
            segmentHeight = segmentHeight,
            overlap = overlap,
            count = count,
        )
    }

    /** 缩放：宽缩到 [maxWidth]（**不放大**），高按比例、四舍五入，都至少 1。 */
    fun scaled(width: Int, height: Int, maxWidth: Int): Pair<Int, Int> = native {
        val ffi = apiScaled(width, height, maxWidth)
        ffi.width to ffi.height
    }

    /** 这张图要不要分段（高 > 宽 × 3 且不算太窄）。 */
    fun isLong(width: Int, height: Int): Boolean = native { apiIsLong(width, height) }

    /** 分段方案（对**缩放之后**的宽高算）。 */
    fun segments(width: Int, height: Int): Segments = native { apiSegments(width, height).toModel() }

    /** 平台给了宽高时这条图分几段；没给（或宽高是 0）就是 null。 */
    fun segmentsFor(width: Int?, height: Int?): Int? = native { apiSegmentsFor(width, height) }

    // ---------------------------------------------------------------- 时间与续页游标

    class ResumeCursorDecoded(val platform: String?, val skip: Int)

    fun decodeResumeCursor(raw: String?): ResumeCursorDecoded = native {
        val ffi = apiResumeCursorDecode(raw)
        ResumeCursorDecoded(ffi.platform, ffi.skip)
    }

    fun encodeResumeCursor(platform: String?, skip: Int): String = native {
        apiResumeCursorEncode(platform, skip)
    }

    fun parseSince(text: String, now: Long = System.currentTimeMillis()): Long = native {
        try {
            apiParseSince(
                text,
                now,
                object : FfiDateToEpoch {
                    override fun toEpochMillis(
                        year: Int,
                        month: UInt,
                        day: UInt,
                        hour: UInt?,
                        minute: UInt?,
                        second: UInt?,
                    ): Long? {
                        val zone = java.time.ZoneId.systemDefault()
                        return runCatching {
                            if (hour == null) {
                                java.time.LocalDate.of(year, month.toInt(), day.toInt())
                                    .atStartOfDay(zone)
                                    .toInstant()
                                    .toEpochMilli()
                            } else {
                                java.time.LocalDateTime.of(
                                    year,
                                    month.toInt(),
                                    day.toInt(),
                                    hour.toInt(),
                                    minute?.toInt() ?: 0,
                                    second?.toInt() ?: 0,
                                )
                                    .atZone(zone)
                                    .toInstant()
                                    .toEpochMilli()
                            }
                        }.getOrNull()
                    }
                },
            )
        } catch (e: FfiApiProtocolException.Failure) {
            throw ProtocolException(e.code, e.detail)
        }
    }

    // ---------------------------------------------------------------- 参数校验

    fun validatePageLimit(raw: String?): Int? = native {
        try {
            apiValidatePageLimit(raw)
        } catch (e: FfiApiProtocolException.Failure) {
            throw ProtocolException(e.code, e.detail)
        }
    }

    fun validateLibraryLimit(raw: String?): Int = native {
        try {
            apiValidateLibraryLimit(raw)
        } catch (e: FfiApiProtocolException.Failure) {
            throw ProtocolException(e.code, e.detail)
        }
    }

    fun validateIntParam(name: String, raw: String?): Int? = native {
        try {
            apiValidateIntParam(name, raw)
        } catch (e: FfiApiProtocolException.Failure) {
            throw ProtocolException(e.code, e.detail)
        }
    }

    class ImageParams(val idx: Int, val maxw: Int, val seg: Int?)

    fun validateImageParams(idx: String?, maxw: String?, seg: String?): ImageParams = native {
        try {
            val ffi = apiValidateImageParams(idx, maxw, seg)
            ImageParams(ffi.idx, ffi.maxw, ffi.seg)
        } catch (e: FfiApiProtocolException.Failure) {
            throw ProtocolException(e.code, e.detail)
        }
    }

    class CommentsParams(val commentId: String?, val pages: Int, val sort: String, val limit: Int?)

    fun validateCommentsParams(
        replies: Boolean,
        comment: String?,
        commentId: String?,
        pages: String?,
        sort: String?,
        limit: String?,
    ): CommentsParams = native {
        try {
            val ffi = apiValidateCommentsParams(replies, comment, commentId, pages, sort, limit)
            CommentsParams(ffi.commentId, ffi.pages, ffi.sort, ffi.limit)
        } catch (e: FfiApiProtocolException.Failure) {
            throw ProtocolException(e.code, e.detail)
        }
    }

    class SearchParams(
        val query: String,
        val platform: String,
        val only: List<String>,
        val page: Int,
        val pages: Int?,
        val kind: String,
        val sort: String,
        val limit: Int?,
    )

    fun validateSearchParams(
        q: String?,
        platform: String?,
        only: String?,
        cursor: String?,
        page: String?,
        pages: String?,
        kind: String?,
        sort: String?,
        limit: String?,
    ): SearchParams = native {
        try {
            val ffi = apiValidateSearchParams(q, platform, only, cursor, page, pages, kind, sort, limit)
            SearchParams(ffi.query, ffi.platform, ffi.only, ffi.page, ffi.pages, ffi.kind, ffi.sort, ffi.limit)
        } catch (e: FfiApiProtocolException.Failure) {
            throw ProtocolException(e.code, e.detail)
        }
    }

    // ---------------------------------------------------------------- 渲染与输出

    fun renderCapabilities(socketPath: String, platformsJson: String): String = native {
        apiRenderCapabilities(socketPath, platformsJson)
    }

    fun renderPlatforms(platformsJson: String, requests: Int): String = native {
        apiRenderPlatforms(platformsJson, requests)
    }

    class PlatformErrorItem(
        val platform: String?,
        val code: String,
        val message: String,
        val retryable: Boolean,
        val extraJson: String? = null,
    )

    fun renderErrorLine(
        code: String,
        platform: String?,
        message: String,
        retryable: Boolean,
        extraJson: String? = null,
        requests: Int? = null,
    ): String = native {
        apiRenderErrorLine(code, platform, message, retryable, extraJson, requests)
    }

    fun renderInlineErrors(errors: List<PlatformErrorItem>, requests: Int): String = native {
        apiRenderInlineErrors(
            errors.map {
                FfiPlatformErrorItem(it.platform, it.code, it.message, it.retryable, it.extraJson)
            },
            requests,
        )
    }

    fun imageReplyHeaders(
        idx: Int,
        width: Int,
        height: Int,
        fullWidth: Int,
        fullHeight: Int,
        segments: Int,
        segment: Int,
        imageCount: Int,
        requests: Int,
    ): Map<String, String> = native {
        apiImageReplyHeaders(
            idx, width, height, fullWidth, fullHeight, segments, segment, imageCount, requests,
        ).associate { it.name to it.value }
    }

    // ---------------------------------------------------------------- 图片清单缓存与正文改写

    fun createImageIndex(detailCapacity: Int = 16, commentCapacity: Int = 64): FfiApiImageIndex = native {
        FfiApiImageIndex(detailCapacity, commentCapacity)
    }

    class DetailJsonResult(val rewrittenJson: String, val images: FfiDetailImages)

    fun rewriteDetailJson(detailJson: String, originalImages: Boolean = true): DetailJsonResult = native {
        try {
            val ffi = apiRewriteDetailJson(detailJson, originalImages)
            DetailJsonResult(ffi.rewrittenJson, ffi.images)
        } catch (e: FfiApiProtocolException.Failure) {
            throw ProtocolException(e.code, e.detail)
        }
    }

    class CommentImagePair(val commentId: String, val images: FfiContentImages)

    fun extractCommentsImages(commentsJson: String, originalImages: Boolean = true): List<CommentImagePair> = native {
        apiExtractCommentsImages(commentsJson, originalImages).map {
            CommentImagePair(it.commentId, it.images)
        }
    }

    // ---------------------------------------------------------------- 搜索混排与续页游标

    class MergedItemInput(val itemKey: String, val itemJson: String)
    class PlatformSliceInput(val platform: String, val items: List<MergedItemInput>, val offset: Int)
    class PageSlicesInput(val page: Int, val slices: List<PlatformSliceInput>)
    class PlatformCursor(val platform: String, val cursor: String?)
    class PageStartCursors(val page: Int, val cursors: List<PlatformCursor>)
    class MergedItemOutput(val itemJson: String, val itemKey: String, val platform: String, val page: Int, val index: Int)
    class SearchMergeOutput(val shown: List<MergedItemOutput>, val truncated: Boolean, val nextJson: String)

    fun mergeSearchResults(
        order: List<String>,
        slicesByPage: List<PageSlicesInput>,
        limit: Int?,
        nextByPlatform: List<PlatformCursor>,
        pageStarts: List<PageStartCursors>,
        firstPage: Int,
        failedPlatforms: List<String>,
    ): SearchMergeOutput = native {
        val ffi = apiMergeSearchResults(
            order = order,
            slicesByPage = slicesByPage.map { p ->
                FfiPageSlicesInput(
                    page = p.page,
                    slices = p.slices.map { s ->
                        FfiPlatformSliceInput(
                            platform = s.platform,
                            items = s.items.map { FfiMergedItemInput(it.itemKey, it.itemJson) },
                            offset = s.offset,
                        )
                    },
                )
            },
            limit = limit,
            nextByPlatform = nextByPlatform.map { FfiPlatformCursor(it.platform, it.cursor) },
            pageStarts = pageStarts.map { ps ->
                FfiPageStartCursors(
                    page = ps.page,
                    cursors = ps.cursors.map { FfiPlatformCursor(it.platform, it.cursor) },
                )
            },
            firstPage = firstPage,
            failedPlatforms = failedPlatforms,
        )
        SearchMergeOutput(
            shown = ffi.shown.map { MergedItemOutput(it.itemJson, it.itemKey, it.platform, it.page, it.index) },
            truncated = ffi.truncated,
            nextJson = ffi.nextJson,
        )
    }

    /**
     * `ApiError` 的 enum 名 → Rust 表里的字符串（`BAD_INPUT` → `bad_input`）。
     *
     * 两边指的是同一件事；表只有一位真相（Rust），名字能对上就说明它们没漂。
     */
    private fun enumKey(name: String): String = name.lowercase()

    private fun FfiApiErrorCode.toModel(): ErrorCode = ErrorCode(code, httpStatus.toInt(), retryable)

    private fun FfiImageSegments.toModel(): Segments =
        Segments(width, height, segmentHeight, overlap, count)

    /**
     * 走 Rust：`.so` 加载失败**不悄悄退回 Kotlin**，给一句说得清的话（和 `ProxyRules` / `RustSettings` 一样）。
     */
    private inline fun <T> native(block: () -> T): T = try {
        RustNative.ensureLoaded()
        block()
    } catch (e: UnsatisfiedLinkError) {
        throw IllegalStateException("对外接口的协议常量在 Rust 里，但 .so 没加载起来：${e.message}", e)
    }
}
