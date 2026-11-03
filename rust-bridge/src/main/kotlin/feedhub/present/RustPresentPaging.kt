package feedhub.present

import feedhub.core.Comment
import feedhub.core.TimePrecision
import feedhub.rust.RustNative
import java.text.DecimalFormatSymbols
import java.time.Instant
import java.time.ZoneId
import uniffi.feedhub_ffi.FfiCardHintsStore
import uniffi.feedhub_ffi.FfiCardImageHint
import uniffi.feedhub_ffi.FfiDetailCache
import uniffi.feedhub_ffi.FfiDetailCacheCommentSeed
import uniffi.feedhub_ffi.FfiDetailCacheEvicted
import uniffi.feedhub_ffi.FfiDetailImagePlan
import uniffi.feedhub_ffi.FfiMediaImageInput
import uniffi.feedhub_ffi.FfiPagingBeginLoad
import uniffi.feedhub_ffi.FfiPagingCompleteResult
import uniffi.feedhub_ffi.FfiPagingEngine
import uniffi.feedhub_ffi.FfiPresentCommentDisplayItem
import uniffi.feedhub_ffi.FfiPresentCommentInput
import uniffi.feedhub_ffi.FfiPresentPreviewReplyDisplayItem
import uniffi.feedhub_ffi.FfiPresentPreviewReplyInput
import uniffi.feedhub_ffi.FfiTimePrecision

/**
 * **详情与分页状态机、会话缓存、快照与缩略图提示的 Rust 门面**。
 *
 * 对应底层模块：
 * - `rust/crates/feedhub-core/src/present/paging.rs`（PagedEngine / 平台评论与楼中楼规则）；
 * - `rust/crates/feedhub-core/src/present/cache.rs`（DetailCache 访问序 LRU 会话缓存）；
 * - `rust/crates/feedhub-core/src/present/snapshot.rs`（FeedSnapshot 磁盘快照读写与过期判定）；
 * - `rust/crates/feedhub-core/src/present/hints.rs`（CardImageHints / DetailThumbnails / 图片排版占位）；
 * - `rust/crates/feedhub-core/src/present/comments.rs`（评论区元数据拼装与一整页评论批量 FFI 预算）。
 */
object RustPresentPaging {
    val DECIMAL_SEP: String by lazy {
        DecimalFormatSymbols.getInstance().decimalSeparator.toString()
    }

    private inline fun <T> load(block: () -> T): T {
        RustNative.ensureLoaded()
        return block()
    }

    // =========================================================================
    // 1. PagedEngine & 平台规则
    // =========================================================================

    fun newPagingEngine(): FfiPagingEngine = load { FfiPagingEngine() }

    fun commentsSupported(platform: String): Boolean =
        load { uniffi.feedhub_ffi.ffiPagingCommentsSupported(platform) }

    fun repliesSupported(capability: Boolean?): Boolean =
        load { uniffi.feedhub_ffi.ffiPagingRepliesSupported(capability) }

    fun commentsSortable(capability: Boolean?): Boolean =
        load { uniffi.feedhub_ffi.ffiPagingCommentsSortable(capability) }

    val COMMENTS_OFF: String by lazy {
        load { uniffi.feedhub_ffi.ffiPagingCommentsOffText() }
    }

    fun shouldSwitchSort(current: String, next: String): Boolean =
        load { uniffi.feedhub_ffi.ffiPagingShouldSwitchSort(current, next) }

    // =========================================================================
    // 2. DetailCache
    // =========================================================================

    fun defaultDetailCache(): FfiDetailCache = load { FfiDetailCache.defaultCache() }

    fun newDetailCache(maxEntries: Int, freshMs: Long): FfiDetailCache =
        load { FfiDetailCache(maxEntries.toUInt(), freshMs) }

    fun detailCacheKey(platform: String, id: String): String =
        load { uniffi.feedhub_ffi.ffiDetailCacheKey(platform, id) }

    // =========================================================================
    // 3. FeedSnapshot
    // =========================================================================

    fun snapshotFileName(kind: String): String =
        load { uniffi.feedhub_ffi.ffiFeedSnapshotFileName(kind) }

    fun saveSnapshot(path: String, savedAt: Long, itemsJson: List<String>): Boolean =
        load { uniffi.feedhub_ffi.ffiFeedSnapshotSave(path, savedAt, itemsJson) }

    fun loadSnapshot(path: String, now: Long): List<String> =
        load { uniffi.feedhub_ffi.ffiFeedSnapshotLoad(path, now) }

    val SNAPSHOT_MAX_ITEMS: Int by lazy {
        load { uniffi.feedhub_ffi.ffiFeedSnapshotMaxItems().toInt() }
    }

    val SNAPSHOT_MAX_AGE_MS: Long by lazy {
        load { uniffi.feedhub_ffi.ffiFeedSnapshotMaxAgeMs() }
    }

    // =========================================================================
    // 4. CardImageHints & DetailThumbnails
    // =========================================================================

    fun newCardHintsStore(): FfiCardHintsStore = load { FfiCardHintsStore() }

    fun cardImageHintRatio(hint: FfiCardImageHint): Float? =
        load { uniffi.feedhub_ffi.ffiCardHintsRatio(hint) }

    fun cardImageSize(media: List<FfiMediaImageInput>, url: String): Pair<Int?, Int?> = load {
        val res = uniffi.feedhub_ffi.ffiCardHintsSize(media, url)
        res.width to res.height
    }

    fun detailThumbnailsHint(hints: List<FfiCardImageHint>, url: String): FfiCardImageHint? =
        load { uniffi.feedhub_ffi.ffiDetailThumbnailsHint(hints, url) }

    fun detailImagePlan(
        url: String,
        original: String?,
        originalImages: Boolean,
        hint: FfiCardImageHint?,
    ): FfiDetailImagePlan =
        load { uniffi.feedhub_ffi.ffiDetailImagePlan(url, original, originalImages, hint) }

    fun detailImageRatio(declared: Float?, hint: FfiCardImageHint?): Float =
        load { uniffi.feedhub_ffi.ffiDetailImageRatio(declared, hint) }

    val DEFAULT_BODY_IMAGE_RATIO: Float by lazy {
        load { uniffi.feedhub_ffi.ffiDefaultBodyImageRatio() }
    }

    // =========================================================================
    // 5. Comments batch & metadata assembly
    // =========================================================================

    fun prepareCommentBatch(
        comments: List<Comment>,
        posterId: String?,
        replyToRootAuthorId: String?,
        onOpenRepliesAllowed: Boolean,
        platformLabel: String?,
        nowMs: Long = System.currentTimeMillis(),
        offsetSec: Long = ZoneId.systemDefault().rules.getOffset(Instant.ofEpochMilli(nowMs)).totalSeconds.toLong(),
        dot: String = DECIMAL_SEP,
    ): List<FfiPresentCommentDisplayItem> = load {
        val ffiInputs = comments.map { it.toFfiInput() }
        uniffi.feedhub_ffi.ffiCommentsPrepareBatch(
            ffiInputs,
            posterId,
            replyToRootAuthorId,
            nowMs,
            offsetSec,
            dot,
            onOpenRepliesAllowed,
            platformLabel,
        )
    }

    fun prepareCommentItem(
        comment: Comment,
        posterId: String?,
        replyToRootAuthorId: String?,
        onOpenRepliesAllowed: Boolean,
        platformLabel: String?,
        nowMs: Long = System.currentTimeMillis(),
        offsetSec: Long = ZoneId.systemDefault().rules.getOffset(Instant.ofEpochMilli(nowMs)).totalSeconds.toLong(),
        dot: String = DECIMAL_SEP,
    ): FfiPresentCommentDisplayItem = load {
        prepareCommentBatch(
            listOf(comment),
            posterId,
            replyToRootAuthorId,
            onOpenRepliesAllowed,
            platformLabel,
            nowMs,
            offsetSec,
            dot,
        ).first()
    }

    fun commentsHeaderTitle(total: Long?, dot: String = DECIMAL_SEP): String =
        load { uniffi.feedhub_ffi.ffiCommentsHeaderTitle(total, dot) }

    fun commentMetaText(floor: Long?, timeText: String?, ipLocation: String?): String =
        load { uniffi.feedhub_ffi.ffiCommentsMetaText(floor, timeText, ipLocation) }

    fun commentMetaText(floor: Int?, timeText: String?, ipLocation: String?): String =
        commentMetaText(floor?.toLong(), timeText, ipLocation)

    fun repliesMoreText(
        total: Long?,
        previewsCount: Int,
        onOpenRepliesAllowed: Boolean,
        platformLabel: String?,
    ): String? = load {
        uniffi.feedhub_ffi.ffiCommentsRepliesMoreText(
            total,
            previewsCount.toUInt(),
            onOpenRepliesAllowed,
            platformLabel,
        )
    }

    fun repliesUnsupportedText(platformLabel: String, pluginInstalled: Boolean): String =
        load { uniffi.feedhub_ffi.ffiCommentsRepliesUnsupportedText(platformLabel, pluginInstalled) }

    private fun Comment.toFfiInput(): FfiPresentCommentInput = FfiPresentCommentInput(
        id = id,
        authorId = author?.id,
        authorName = author?.name,
        likes = likes,
        floor = floor?.toLong(),
        publishedAt = publishedAt,
        publishedAtPrecision = publishedAtPrecision?.toFfi(),
        ipLocation = ipLocation,
        replyToId = replyTo?.id,
        replyToName = replyTo?.name,
        replyCount = replyCount,
        previewReplies = previewReplies.take(3).map { pr ->
            FfiPresentPreviewReplyInput(
                authorId = pr.author?.id,
                authorName = pr.author?.name,
                replyToId = pr.replyTo?.id,
                replyToName = pr.replyTo?.name,
            )
        },
    )

    private fun TimePrecision.toFfi(): FfiTimePrecision = when (this) {
        TimePrecision.SECOND -> FfiTimePrecision.SECOND
        TimePrecision.MINUTE -> FfiTimePrecision.MINUTE
        TimePrecision.HOUR -> FfiTimePrecision.HOUR
        TimePrecision.DAY -> FfiTimePrecision.DAY
        TimePrecision.MONTH -> FfiTimePrecision.MONTH
        TimePrecision.YEAR -> FfiTimePrecision.YEAR
    }
}
