package feedhub.app.detail

import androidx.compose.ui.unit.dp
import feedhub.core.Block
import feedhub.present.RustPresentImages

/**
 * 正文图片的排布规则（纯计算在 Rust，门面保留在此以便 Compose 界面与常量复用）。
 */

/** 宫格最多画几格：超出的一张在**第 9 格**上叠 "+N"（N = 没画出来的张数）。 */
internal const val MaxGridCells = 9

/**
 * 详情正文的宽度口径：
 * 内容最宽 [DetailContentMaxWidth]、两侧至少 [DetailSidePadding]，格子和格子之间 [GridGap]。
 */
internal val DetailContentMaxWidth = 760.dp
internal val DetailSidePadding = 16.dp
internal val GridGap = 4.dp

/**
 * 宽高比（宽 / 高）的极端值夹取。
 */
internal const val MinBodyImageRatio = 0.5f
internal const val MaxBodyImageRatio = 3f

/** 没给宽高时的占位比例（4:3）。 */
internal const val DefaultBodyImageRatio = 4f / 3f

/**
 * 宫格的列数：1 张照旧走单图那条路（不进 [imageGridColumns]）；2–4 张两列；≥5 张三列。
 */
internal fun imageGridColumns(count: Int): Int = RustPresentImages.imageGridColumns(count)

/** 宫格里实际画几个格子（最多 [MaxGridCells]，多出来的用 "+N" 表示）。 */
internal fun imageGridCells(count: Int): Int = RustPresentImages.imageGridCells(count)

/** 第 9 格上要叠的 "+N"：没超出就是 0（不叠）。 */
internal fun imageGridMore(count: Int): Int = RustPresentImages.imageGridMore(count)

/**
 * 正文里**单张**图按宽度 [width] 排出来多高。
 */
internal fun bodyImageHeight(width: Float, ratio: Float, maxHeight: Float): BodyImageBox {
    val res = RustPresentImages.bodyImageHeight(width, ratio, maxHeight)
    return BodyImageBox(res.height, res.cropped, res.fitWidth)
}

/** 批量计算正文单图高度。 */
internal fun bodyImageHeightBatch(
    widths: List<Float>,
    ratios: List<Float>,
    maxHeights: List<Float>,
): List<BodyImageBox> =
    RustPresentImages.bodyImageHeightBatch(widths, ratios, maxHeights).map {
        BodyImageBox(it.height, it.cropped, it.fitWidth)
    }

/**
 * **单张图**（不是宫格）在给定容器宽度里的排布。
 */
internal fun fittedImageBox(width: Float, ratio: Float, maxHeight: Float): BodyImageBox =
    bodyImageHeight(width, ratio, maxHeight)

/**
 * [bodyImageHeight] 的结果：要画多高。
 */
internal data class BodyImageBox(val height: Float, val cropped: Boolean, val fitWidth: Boolean = false) {
    /** 这个框里图片的宽度：[fitWidth] 时按比例算（比例由调用方另给），否则就是容器宽。 */
    fun widthFor(containerWidth: Float, ratio: Float): Float =
        RustPresentImages.BodyImageBox(height, cropped, fitWidth).widthFor(containerWidth, ratio)
}

/** 图片块声明的宽高比；没给（或给了 0 / 负数）就是 null。 */
internal fun Block.Image.declaredRatio(): Float? = RustPresentImages.declaredRatio(width, height)

/**
 * 一个渲染单元：单独一块，或者一组**连续**的图片（画成宫格）。
 */
internal class RichUnit(val firstIndex: Int, val blocks: List<Block>) {
    /** 这一组是不是宫格（连续 2 张以上图片）。 */
    val isGrid: Boolean get() = blocks.size > 1
}

/** 正文块 → 渲染单元：连续的图片（≥2 张）收成一组画宫格，别的块各算一个；1 张图照旧单独一块。整篇 1 次 FFI。 */
internal fun richUnits(blocks: List<Block>): List<RichUnit> {
    if (blocks.isEmpty()) return emptyList()
    val isImages = blocks.map { it is Block.Image }
    val ranges = RustPresentImages.richUnits(isImages)
    return ranges.map { r ->
        RichUnit(r.firstIndex, blocks.subList(r.firstIndex, r.firstIndex + r.len))
    }
}
