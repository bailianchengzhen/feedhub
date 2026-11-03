package feedhub.app.feed

import androidx.compose.runtime.Immutable
import feedhub.core.FeedItem
import feedhub.core.Media
import feedhub.present.RustPresentPaging
import uniffi.feedhub_ffi.FfiCardImageHint

/**
 * 卡片上那张图**当时用的地址**：
 * 首页 / 关注 / 收藏 / 历史的卡片都用缩略图，详情页进来时拿同一份地址当占位。
 *
 * 去重与 LRU 状态机在 Rust（[RustPresentPaging]），纯数值换算留在 Kotlin 免卡片组合路径过 FFI。
 */
@Immutable
data class CardImageHint(
    /** 卡片上那张图的原始地址。 */
    val source: String,
    /** 卡片实际请求的那份地址。 */
    val thumbnail: String,
    val width: Int? = null,
    val height: Int? = null,
) {
    /** 真实宽高比（宽 / 高）：详情里没有声明宽高时先按它把位置占好；不知道就是 null。 */
    val ratio: Float?
        get() {
            val w = width ?: return null
            val h = height ?: return null
            return if (w > 0 && h > 0) w.toFloat() / h.toFloat() else null
        }

    internal fun toFfi(): FfiCardImageHint =
        FfiCardImageHint(source, thumbnail, width, height)

    companion object {
        internal fun fromFfi(ffi: FfiCardImageHint): CardImageHint =
            CardImageHint(ffi.source, ffi.thumbnail, ffi.width, ffi.height)
    }
}

/**
 * 卡片上那张图 + 它在卡片上实际请求的地址。卡片上没有图就是 null。
 */
internal fun cardImageHint(item: FeedItem, cardWidthPx: Int): CardImageHint? {
    val spec = cardImageSpec(item) ?: return null
    val (w, h) = cardImageSize(item, spec.url)
    return CardImageHint(source = spec.url, thumbnail = cardImageUrl(spec.url, cardWidthPx), width = w, height = h)
}

/** [url] 在平台数据里的像素宽高（找不到就是两个 null；与 `present::hints::card_image_size` 同式，0 FFI）。 */
private fun cardImageSize(item: FeedItem, url: String): Pair<Int?, Int?> {
    for (m in item.media) {
        when (m) {
            is Media.Image -> if (m.url == url) return m.width to m.height
            is Media.Video -> if (m.cover.orEmpty() == url) return m.width to m.height
        }
    }
    return null to null
}

/**
 * 这一趟里卡片上那几张图的地址。访问序 LRU，上限 500 条内容。
 * 核心状态机与图片 identity 去重下沉至 Rust（[FfiCardHintsStore]），Kotlin 侧去重相同记录免重复过 FFI。
 */
object CardImageHints {
    const val MAX_ENTRIES = 500
    const val MAX_PER_ITEM = 9

    private val store = RustPresentPaging.newCardHintsStore()
    private val dedupLock = Any()
    private val lastSingle = object : LinkedHashMap<String, CardImageHint>(256, 0.75f, true) {
        override fun removeEldestEntry(eldest: MutableMap.MutableEntry<String, CardImageHint>?): Boolean =
            size > MAX_ENTRIES
    }
    private val lastGrid = object : LinkedHashMap<String, List<CardImageHint>>(256, 0.75f, true) {
        override fun removeEldestEntry(eldest: MutableMap.MutableEntry<String, List<CardImageHint>>?): Boolean =
            size > MAX_ENTRIES
    }

    fun record(itemKey: String, hint: CardImageHint) {
        synchronized(dedupLock) {
            if (lastSingle[itemKey] == hint) return
            lastSingle[itemKey] = hint
        }
        store.record(itemKey, hint.toFfi())
    }

    fun recordGrid(itemKey: String, hints: List<CardImageHint>) {
        if (hints.isEmpty()) return
        synchronized(dedupLock) {
            if (lastGrid[itemKey] == hints) return
            lastGrid[itemKey] = hints
        }
        store.recordGrid(itemKey, hints.map { it.toFfi() })
    }

    fun of(itemKey: String): List<CardImageHint> =
        store.of(itemKey).map { CardImageHint.fromFfi(it) }

    val size: Int get() = store.size().toInt()

    fun clear() {
        synchronized(dedupLock) {
            lastSingle.clear()
            lastGrid.clear()
        }
        store.clear()
    }
}
