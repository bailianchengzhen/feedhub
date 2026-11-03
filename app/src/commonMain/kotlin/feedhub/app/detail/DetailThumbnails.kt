package feedhub.app.detail

import androidx.compose.runtime.Immutable
import androidx.compose.runtime.compositionLocalOf
import feedhub.app.feed.CardImageHint
import feedhub.app.feed.CardImageHints
import feedhub.core.Block
import feedhub.present.RustPresentPaging

/**
 * 详情页里可用的"卡片上已经下好的那张缩略图"。
 *
 * 匹配计算与规划均下沉至 Rust（[RustPresentPaging]）。
 */
@Immutable
class DetailThumbnails private constructor(private val hints: List<CardImageHint>) {
    val count: Int get() = hints.size

    fun hint(url: String): CardImageHint? {
        if (hints.isEmpty()) return null
        val ffiHint = RustPresentPaging.detailThumbnailsHint(hints.map { it.toFfi() }, url) ?: return null
        return CardImageHint.fromFfi(ffiHint)
    }

    fun hint(block: Block.Image): CardImageHint? = hint(block.url) ?: block.original?.let(::hint)

    val cardImage: CardImageHint? get() = hints.firstOrNull()

    override fun equals(other: Any?): Boolean = other is DetailThumbnails && other.hints == hints

    override fun hashCode(): Int = hints.hashCode()

    companion object {
        val EMPTY = DetailThumbnails(emptyList())

        fun of(hints: Iterable<CardImageHint>): DetailThumbnails {
            val list = hints.toList()
            return if (list.isEmpty()) EMPTY else DetailThumbnails(list)
        }

        fun of(itemKey: String): DetailThumbnails = of(CardImageHints.of(itemKey))
    }
}

val LocalDetailThumbnails = compositionLocalOf { DetailThumbnails.EMPTY }

internal fun detailImagePlan(
    url: String,
    original: String?,
    originalImages: Boolean,
    hint: CardImageHint?,
): DetailImagePlan {
    val plan = RustPresentPaging.detailImagePlan(url, original, originalImages, hint?.toFfi())
    return DetailImagePlan(plan.url, plan.placeholder)
}

internal data class DetailImagePlan(val url: String, val placeholder: String?)

internal fun detailImageRatio(declared: Float?, hint: CardImageHint?): Float =
    RustPresentPaging.detailImageRatio(declared, hint?.toFfi())
