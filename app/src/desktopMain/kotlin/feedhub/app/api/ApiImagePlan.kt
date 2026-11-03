package feedhub.app.api

import feedhub.core.Block
import feedhub.core.Comment
import feedhub.core.FeedDetail
import feedhub.core.FeedJson
import feedhub.rust.RustAppApi
import kotlinx.serialization.json.JsonElement
import uniffi.feedhub_ffi.FfiApiImage
import uniffi.feedhub_ffi.FfiContentImages
import uniffi.feedhub_ffi.FfiDetailImages

/** 视频帖那条"封面"块的说明：它是一张图，**不是**视频画面。 */
internal const val VIDEO_COVER_NOTE: String = "这是视频的封面图，不是视频画面本身（视频要在 App 里看）"

/**
 * 一张图（服务端内部用）。[url] **只用来取图，任何输出里都不出现**。
 *
 * [width] / [height] 是平台（或封面）声明的像素尺寸，可能没有；[isCover] = 封面 / 首图。
 */
internal class ApiImage(
    val url: String,
    val width: Int? = null,
    val height: Int? = null,
    val isCover: Boolean = false,
    val note: String? = null,
)

/** 一条内容（一条详情 / 一条评论）的图片清单：[entries] 的下标就是对外的 `idx`。 */
internal class ContentImages(val entries: List<ApiImage>) {
    val count: Int get() = entries.size

    fun at(idx: Int): ApiImage? = entries.getOrNull(idx)

    /**
     * 这张图要分几段（按默认 1024 宽缩放后算；比例不变，所以段数和 `maxw` 基本无关）。
     * 宽高拿不到 → null（能算就是 ≥ 1）。
     */
    fun segmentsAt(idx: Int): Int? {
        val image = at(idx) ?: return null
        val w = image.width?.takeIf { it > 0 } ?: return null
        val h = image.height?.takeIf { it > 0 } ?: return null
        return RustAppApi.segmentsFor(w, h)
    }
}

/** 一条**详情**的图片清单：[images] + 视频帖要补在最前面的封面块 [coverBlock]。 */
internal class DetailImages(
    val images: ContentImages,
    val coverBlock: ApiImage?,
) {
    /** `body.blocks` 里第几个 `img` 块（0 起，只数 img）→ [images] 的下标。 */
    private val imgBlockOrder: Map<Int, Int> = run {
        val map = LinkedHashMap<Int, Int>()
        var seen = 0
        for ((index, image) in images.entries.withIndex()) {
            if (image === coverBlock) continue
            map[seen++] = index
        }
        map
    }

    /**
     * 正文里**第几个** `img` 块（0 起，只数 `img`，不是 `blocks` 的下标）→（`idx`, 那张图）。
     * 对不上就是 null（正常不会）。
     */
    fun imageForImgBlock(imgOrdinal: Int): Pair<Int, ApiImage>? {
        val idx = imgBlockOrder[imgOrdinal] ?: return null
        return idx to images.entries[idx]
    }
}

internal fun ApiImage.toFfi(): FfiApiImage = FfiApiImage(url, width, height, isCover, note)
internal fun FfiApiImage.toModel(): ApiImage = ApiImage(url, width, height, isCover, note)

internal fun ContentImages.toFfi(): FfiContentImages = FfiContentImages(entries.map { it.toFfi() })
internal fun FfiContentImages.toModel(): ContentImages = ContentImages(entries.map { it.toModel() })

internal fun DetailImages.toFfi(): FfiDetailImages = FfiDetailImages(images.toFfi(), coverBlock != null)
internal fun FfiDetailImages.toModel(): DetailImages {
    val content = images.toModel()
    val cover = if (hasCover) content.entries.firstOrNull() else null
    return DetailImages(content, cover)
}

/**
 * 一条详情里的图片清单（按对外的 `idx` 顺序）。
 * 搬迁至 Rust（`feedhub-core::appapi::detail_json`）。
 */
internal fun detailImages(detail: FeedDetail, originalImages: Boolean = true): DetailImages {
    val json = FeedJson.encodeToString(FeedDetail.serializer(), detail)
    return RustAppApi.rewriteDetailJson(json, originalImages).images.toModel()
}

/**
 * 一条评论里的图片清单（`/image?comment=` 用；`idx` 是**这条评论里**的序号）。
 * 搬迁至 Rust（`feedhub-core::appapi::detail_json`）。
 */
internal fun commentImages(comment: Comment, originalImages: Boolean = true): ContentImages {
    val json = FeedJson.encodeToString(Comment.serializer(), comment)
    val pairs = RustAppApi.extractCommentsImages(json, originalImages)
    return pairs.firstOrNull()?.images?.toModel() ?: ContentImages(emptyList())
}

/** 正文图该请求哪个地址：和 App 的 `RichTextView` 一样（"默认显示原图"开着就用 `original`）。 */
internal fun imageUrlOf(block: Block.Image, originalImages: Boolean): String =
    if (originalImages) block.original?.takeIf { it.isNotBlank() } ?: block.url else block.url

/**
 * `/detail` 的单次改写入口：一次序列化 + 一次 Rust 调用同时拿到 [DetailImages] 与改写后的 [JsonElement]。
 */
internal fun rewriteDetail(detail: FeedDetail, originalImages: Boolean = true): Pair<DetailImages, JsonElement> {
    val json = FeedJson.encodeToString(FeedDetail.serializer(), detail)
    val res = RustAppApi.rewriteDetailJson(json, originalImages)
    return res.images.toModel() to FeedJson.parseToJsonElement(res.rewrittenJson)
}

/**
 * `/detail` 的输出：`FeedDetail` 的序列化 + 图片那套改写。
 * 搬迁至 Rust（`feedhub-core::appapi::detail_json`）。
 */
internal fun detailJson(detail: FeedDetail, images: DetailImages): JsonElement {
    val json = FeedJson.encodeToString(FeedDetail.serializer(), detail)
    val rewritten = RustAppApi.rewriteDetailJson(json).rewrittenJson
    return FeedJson.parseToJsonElement(rewritten)
}

/** 一条评论 + 它下面的楼中楼预览里所有评论（灌图片清单缓存用）。 */
internal fun commentsWithPreviews(comments: List<Comment>): List<Comment> {
    val out = ArrayList<Comment>(comments.size)
    fun visit(list: List<Comment>) {
        for (c in list) {
            out += c
            if (c.previewReplies.isNotEmpty()) visit(c.previewReplies)
        }
    }
    visit(comments)
    return out
}
