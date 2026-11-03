package feedhub.app.video

import feedhub.app.DefaultQuality
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.isSpecified

/**
 * 正文里夹着的视频（[InlineVideo]）的几个可调的值，想改行为只改这里。
 */
object InlineVideoDefaults {
    /**
     * 滚到可见时自动（静音）播放。默认 false：先显示封面和播放键，点一下才播。
     * 以后想改成自动播放，把它改成 true 就行（[InlineVideo] 的 `autoplayWhenVisible` 参数默认取它）；自动播放时是静音开播，放大看的时候才有声音。
     */
    const val AUTOPLAY_WHEN_VISIBLE = false

    /** 嵌入状态下默认清晰度的上限（省资源）；放大看时按设置里的默认清晰度，见 [PlaybackSession.qualityLimit]。 */
    val EMBEDDED_QUALITY = DefaultQuality.P720

    /** 嵌入的视频最高多高：太高的竖屏视频限制在这里（宽度按比例跟着缩）。 */
    val MAX_HEIGHT = 520.dp

    /** 可见部分不到这个比例就算滚出去了：暂停并释放播放器。 */
    const val STOP_BELOW = 0.25f

    /** 自动播放（[AUTOPLAY_WHEN_VISIBLE]）时，可见部分到这个比例才开播。 */
    const val AUTOPLAY_ABOVE = 0.6f

    /** 播完离结尾不到这么久（毫秒）就当看完了，下次点开从头播而不是只剩一小截。 */
    const val RESUME_END_MARGIN_MS = 1500L

    /** 画面比例的合理范围：再扁 / 再高的当成这个范围的边界（宽高数据不对时不至于撑出一条细缝）。 */
    const val MIN_RATIO = 0.3f
    const val MAX_RATIO = 3f
}

/**
 * 嵌入框的宽高比：播放器报出了真实画面尺寸就用真实的（画面正好填满，没有黑边），否则用 [Block.Video][feedhub.core.Block.Video] 里声明的宽高，
 * 都没有就 16:9。结果收在 [InlineVideoDefaults.MIN_RATIO]~[InlineVideoDefaults.MAX_RATIO] 里。
 */
fun inlineVideoRatio(declaredWidth: Int?, declaredHeight: Int?, actualWidth: Int = 0, actualHeight: Int = 0): Float {
    val ratio = when {
        actualWidth > 0 && actualHeight > 0 -> actualWidth.toFloat() / actualHeight
        declaredWidth != null && declaredHeight != null && declaredWidth > 0 && declaredHeight > 0 -> declaredWidth.toFloat() / declaredHeight
        else -> 16f / 9f
    }
    return ratio.coerceIn(InlineVideoDefaults.MIN_RATIO, InlineVideoDefaults.MAX_RATIO)
}

/**
 * 嵌入框的宽度：不超过容器宽 [available]，不超过 [maxWidth]（评论里的视频不用撑满，没给就不限；和正文里的图片一样），
 * 高度（宽 / [ratio]）也不超过 [maxHeight]——太高的竖屏视频按高度封顶，宽度跟着缩。高度由调用方按 [ratio] 算。
 */
fun inlineVideoWidth(available: Dp, maxWidth: Dp, ratio: Float, maxHeight: Dp = InlineVideoDefaults.MAX_HEIGHT): Dp {
    val limit = if (maxWidth.isSpecified) minOf(available, maxWidth) else available
    return minOf(limit, maxHeight * ratio)
}

/**
 * 嵌入框可见的比例（0~1）：[visibleWidth] × [visibleHeight] 是被所有祖先裁剪之后还看得见的那一块（像素），[width] × [height] 是它的完整尺寸。
 * 完整尺寸为 0 时当成看得见（还没排版好，别误判成滚出去了）。
 */
fun visibleFraction(visibleWidth: Int, visibleHeight: Int, width: Int, height: Int): Float {
    if (width <= 0 || height <= 0) return 1f
    val visible = visibleWidth.coerceIn(0, width).toLong() * visibleHeight.coerceIn(0, height)
    return visible.toFloat() / (width.toLong() * height)
}

/** 可见部分不到 [InlineVideoDefaults.STOP_BELOW]：该暂停并释放了。 */
fun shouldStopWhenHidden(fraction: Float): Boolean = fraction < InlineVideoDefaults.STOP_BELOW

/**
 * 该不该自动开播：开关（[enabled]）开着、看得见的部分够多、现在没有别的行内视频在播（[activeKey] 为 null）。
 * 同一时间只有一个在播，所以同时露出好几个时先到的先播。
 */
fun shouldAutoplay(enabled: Boolean, fraction: Float, activeKey: String?): Boolean =
    enabled && activeKey == null && fraction >= InlineVideoDefaults.AUTOPLAY_ABOVE

/** 释放前播到 [positionMs]（总长 [durationMs]），下次点开从哪开始：快播完了（或不知道总长、位置不对）就从头。 */
fun resumeStart(positionMs: Long, durationMs: Long): Long = when {
    positionMs <= 0 -> 0L
    durationMs > 0 && positionMs >= durationMs - InlineVideoDefaults.RESUME_END_MARGIN_MS -> 0L
    else -> positionMs
}
