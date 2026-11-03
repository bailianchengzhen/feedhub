package feedhub.app.display

/**
 * 一次向系统表达的刷新率请求。
 *
 * 不按场景调**（原话"刷新率锁最高不要动"）：App 在前台就一直用屏幕能到的最高刷新率，
 * 不按滚动 / 视频帧率 / 空闲去调高调低。安卓上落到 `WindowManager.LayoutParams.preferredDisplayModeId`
 * （挑当前分辨率下刷新率最高的显示模式，见 `androidMain` 的 `WindowFrameRateSink`）；
 * 桌面不接（[NoFrameRateSink]），行为不变。
 *
 * @param max true = 请系统用最高的刷新率；false = 撤回（回到系统自己的策略，退到后台时用）。
 */
fun interface FrameRateSink {
    fun apply(max: Boolean)
}

/** 不接系统的实现（桌面版、离线测试、截图）：一个系统调用也不发，桌面行为完全不变。 */
val NoFrameRateSink: FrameRateSink = FrameRateSink { }

/**
 * 刷新率的状态机：**只有一条规则**——App 在前台就要最高的刷新率，不随滚动 / 播放 / 空闲变化
 *
 * 1. 前台 → 请求最高刷新率（[setForeground]`(true)`）；
 * 2. 退到后台 → 撤回（后台没有画面要看，不占着高刷；系统自己也会按它的策略处理）；
 * 3. 回到前台 → 重新要最高。
 *
 * 只在状态**真的变了**时调 [sink]（重复的 onStart / onStop 不会反复打扰系统）。
 */
class FrameRateStateMachine(private val sink: FrameRateSink = NoFrameRateSink) {
    /** App 在前台吗（安卓由 Activity 的 onStart / onStop 喂；桌面不接，永远 false = 不发任何系统调用）。 */
    var foreground: Boolean = false
        private set

    /** 现在有没有向系统要最高刷新率。 */
    var maxed: Boolean = false
        private set

    fun setForeground(active: Boolean) {
        if (foreground == active) return
        foreground = active
        apply(active)
    }

    private fun apply(on: Boolean) {
        if (maxed == on) return
        maxed = on
        sink.apply(on)
    }
}
