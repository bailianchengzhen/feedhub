package feedhub.app.image

/**
 * 动图的时间轴：每一帧显示多久、一共播几遍，以及"播放到第 t 毫秒时该显示哪一帧"。纯逻辑，不依赖 Skia，有单元测试。
 *
 * @param durationsMs 每一帧的显示时长（毫秒，已经按 [normalizeDelay] 处理过，都 > 0）
 * @param plays 一共播几遍，0 = 一直循环；播完停在最后一帧
 */
internal class FrameTimeline(durationsMs: IntArray, val plays: Int) {
    init {
        require(durationsMs.isNotEmpty()) { "动图至少要有一帧" }
        require(durationsMs.all { it > 0 }) { "每帧时长必须大于 0" }
        require(plays >= 0)
    }

    val frameCount: Int = durationsMs.size

    /** 第 i 帧结束的时刻（从一遍的开头算）。 */
    private val ends = LongArray(frameCount).also { var t = 0L; for (i in durationsMs.indices) { t += durationsMs[i]; it[i] = t } }

    /** 一遍的总时长（毫秒）。 */
    val cycleMs: Long = ends.last()

    /** 播放了 [positionMs] 毫秒之后，是不是已经播完（有限次数循环才会）。 */
    fun isFinished(positionMs: Long): Boolean = plays > 0 && positionMs >= cycleMs * plays

    /** 播放到 [positionMs] 毫秒时该显示第几帧；播完停在最后一帧。 */
    fun frameAt(positionMs: Long): Int {
        val p = positionMs.coerceAtLeast(0)
        if (isFinished(p)) return frameCount - 1
        val t = p % cycleMs
        // 第一个结束时刻 > t 的帧
        var lo = 0
        var hi = frameCount - 1
        while (lo < hi) {
            val mid = (lo + hi) ushr 1
            if (ends[mid] > t) hi = mid else lo = mid + 1
        }
        return lo
    }

    /** 播放到 [positionMs] 毫秒时，离换下一帧还有几毫秒（至少 1）；已经播完返回 [Long.MAX_VALUE]。 */
    fun msUntilNextFrame(positionMs: Long): Long {
        val p = positionMs.coerceAtLeast(0)
        if (isFinished(p)) return Long.MAX_VALUE
        val t = p % cycleMs
        return (ends[frameAt(p)] - t).coerceAtLeast(1)
    }

    companion object {
        /** 没写时长、或者写得太小的帧按这个算。 */
        const val DEFAULT_DELAY_MS = 100

        /** 不管哪种格式，一帧最短显示这么久（再短也画不出来，免得把动画线程跑满）。 */
        const val MIN_DELAY_MS = 10

        /**
         * 帧时长的处理，照浏览器的惯例：
         * - GIF：很多 GIF 把帧时长写成 0 或 10 毫秒（"尽快"），浏览器（Firefox / Chrome）对 <= 10 毫秒的一律当 100 毫秒；
         * - WebP：0 当 100 毫秒，别的照写的，但不低于 [MIN_DELAY_MS]。
         */
        fun normalizeDelay(rawMs: Int, format: AnimatedFormat): Int = when (format) {
            AnimatedFormat.GIF -> if (rawMs <= 10) DEFAULT_DELAY_MS else rawMs
            AnimatedFormat.WEBP -> if (rawMs <= 0) DEFAULT_DELAY_MS else rawMs.coerceAtLeast(MIN_DELAY_MS)
        }

        /**
         * Skia 的 `Codec.repetitionCount`：-1 = 一直循环，n >= 0 = 第一遍之外再重复 n 遍（GIF 没写循环扩展就是 0，只播一遍）。
         * 换成"一共播几遍"，0 = 一直循环。
         */
        fun playsOf(repetitionCount: Int): Int = if (repetitionCount < 0) 0 else repetitionCount + 1
    }
}
