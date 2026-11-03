package feedhub.app.api

import feedhub.rust.RustAppApi

/**
 * 图片几何：**纯计算，离线可测**。
 *
 * **规则在 Rust**：
 * 1. **缩放**：把图缩到宽 ≤ `maxw`（不放大）；
 * 2. **长图分段**：高 > 宽 × 3 的图（长截图、长图）缩成一条会糊，按"每段高 ≈ 缩放后宽 × 1.5、段间留 8% 重叠"切开。
 *    段数只和图的长宽比有关（比例不变），所以 `/detail` 能用平台声明的宽高先算一遍给 agent、`/image` 再用
 *    **实际解出来的**宽高算一遍。
 *
 * 这里只留"接口形状"（`scaled` 返回 `Pair`、`segments` 返回一个能 `top` / `heightAt` 的对象），
 * 数字一个都不写：常量都在 Rust（`RustAppApi.imageLimits`），改口径只改一处。
 */
internal object ImageGeometry {
    /** `maxw` 默认值（`RustAppApi.imageLimits.defaultMaxWidth`）。 */
    const val DEFAULT_MAX_WIDTH: Int = 1024

    /** `maxw` 上限（`RustAppApi.imageLimits.maxMaxWidth`）。 */
    val MAX_MAX_WIDTH: Int get() = RustAppApi.imageLimits.maxMaxWidth

    /** 高 > 宽 × 3 = 长图（`RustAppApi.imageLimits.longRatio`）。 */
    val LONG_RATIO: Int get() = RustAppApi.imageLimits.longRatio

    /** 缩放后的宽高：宽缩到 [maxWidth]（**不放大**），高按比例；至少 1。Rust 算。 */
    fun scaled(width: Int, height: Int, maxWidth: Int): Pair<Int, Int> =
        RustAppApi.scaled(width, height, maxWidth)

    /** 这张（已经缩过的）图要不要分段：高 > 宽 × 3，而且不算太窄。Rust 算。 */
    fun isLong(width: Int, height: Int): Boolean = RustAppApi.isLong(width, height)

    /**
     * 分段方案（对**缩放之后**的 [width] × [height] 算）。Rust 算。
     *
     * `count == 1` 表示不分段（这时 `segmentHeight` = 整张高、`overlap` = 0）。
     */
    fun segments(width: Int, height: Int): Segments = Segments(RustAppApi.segments(width, height))

    /**
     * 一张图的分段方案。`top(i)` 是第 [index] 段从缩放后的第几行开始，`heightAt(i)` 是它有多高
     * （最后一段可能比 `segmentHeight` 短）。
     */
    class Segments internal constructor(private val inner: RustAppApi.Segments) {
        val width: Int get() = inner.width
        val height: Int get() = inner.height
        val segmentHeight: Int get() = inner.segmentHeight
        val overlap: Int get() = inner.overlap
        val count: Int get() = inner.count

        /** 第 [index] 段在缩放后图片里的起始行。 */
        fun top(index: Int): Int = inner.top(index)

        /** 第 [index] 段有多少行（最后一段可能短）。 */
        fun heightAt(index: Int): Int = inner.heightAt(index)
    }
}
