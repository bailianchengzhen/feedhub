package feedhub.present

import feedhub.rust.RustNative
import uniffi.feedhub_ffi.presentBodyImageHeight
import uniffi.feedhub_ffi.presentBodyImageHeightBatch
import uniffi.feedhub_ffi.presentBodyImageWidthFor
import uniffi.feedhub_ffi.presentCommentImageUrl
import uniffi.feedhub_ffi.presentCommentImageUrlBatch
import uniffi.feedhub_ffi.presentCommentThumbCells
import uniffi.feedhub_ffi.presentCommentThumbMore
import uniffi.feedhub_ffi.presentDeclaredRatio
import uniffi.feedhub_ffi.presentImageGridCells
import uniffi.feedhub_ffi.presentImageGridColumns
import uniffi.feedhub_ffi.presentImageGridMore
import uniffi.feedhub_ffi.presentRichUnits

/**
 * **正文与评论图片排布纯函数的 Rust 门面**。
 *
 * 规则本体在 `rust/crates/feedhub-core/src/present/images.rs`。
 * 这里只做类型转换 + 确保 `.so` 在。没有开关、没有 Kotlin 备用实现。
 */
object RustPresentImages {
    data class BodyImageBox(val height: Float, val cropped: Boolean, val fitWidth: Boolean = false) {
        fun widthFor(containerWidth: Float, ratio: Float): Float = native {
            presentBodyImageWidthFor(height, fitWidth, containerWidth, ratio)
        }
    }

    data class RichUnitRange(val firstIndex: Int, val len: Int)

    fun imageGridColumns(count: Int): Int = native { presentImageGridColumns(count) }

    fun imageGridCells(count: Int): Int = native { presentImageGridCells(count) }

    fun imageGridMore(count: Int): Int = native { presentImageGridMore(count) }

    fun bodyImageHeight(width: Float, ratio: Float, maxHeight: Float): BodyImageBox = native {
        val ffi = presentBodyImageHeight(width, ratio, maxHeight)
        BodyImageBox(ffi.height, ffi.cropped, ffi.fitWidth)
    }

    fun bodyImageHeightBatch(
        widths: List<Float>,
        ratios: List<Float>,
        maxHeights: List<Float>,
    ): List<BodyImageBox> = native {
        presentBodyImageHeightBatch(widths, ratios, maxHeights).map {
            BodyImageBox(it.height, it.cropped, it.fitWidth)
        }
    }

    fun fittedImageBox(width: Float, ratio: Float, maxHeight: Float): BodyImageBox =
        bodyImageHeight(width, ratio, maxHeight)

    fun declaredRatio(width: Int?, height: Int?): Float? = native {
        presentDeclaredRatio(width, height)
    }

    fun richUnits(isImageBlock: List<Boolean>): List<RichUnitRange> = native {
        presentRichUnits(isImageBlock).map {
            RichUnitRange(it.firstIndex.toInt(), it.len.toInt())
        }
    }

    fun commentThumbCells(count: Int): Int = native { presentCommentThumbCells(count) }

    fun commentThumbMore(count: Int): Int = native { presentCommentThumbMore(count) }

    fun commentImageUrl(url: String, widthPx: Int): String = native {
        presentCommentImageUrl(url, widthPx)
    }

    fun commentImageUrlBatch(urls: List<String>, widthPxs: List<Int>): List<String> = native {
        presentCommentImageUrlBatch(urls, widthPxs)
    }

    private inline fun <T> native(block: () -> T): T = try {
        RustNative.ensureLoaded()
        block()
    } catch (e: UnsatisfiedLinkError) {
        throw IllegalStateException("图片排布在 Rust 里，但 .so 没加载起来：${e.message}", e)
    }
}
