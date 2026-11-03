package feedhub.app

/**
 * 系统级动作：系统分享面板、「在浏览器打开」、官方 App 深链。
 *
 * 桌面上这些动作**一个也不存在**（[None]）：分享还是"复制标题和链接"、外链还是"复制链接"，
 * 安卓在 `androidGraph` 里给
 * `AndroidSystemActions`（`ACTION_SEND` + chooser、Custom Tabs、`Intent.ACTION_VIEW` 深链）。
 *
 * 每个方法都返回"这件事真的做成了吗"：**失败要退化成复制**，不能点了什么都不发生
 * （`resolveActivity` 找不到时系统直接抛 `ActivityNotFoundException`，安卓那边接住返回 false）。
 */
interface SystemActions {
    /** 这一端有系统界面可调（安卓 true，桌面 / 演示 / 测试 false）。为 false 时界面上不出现"在 xx 打开"。 */
    val available: Boolean

    /** 弹系统分享面板（`ACTION_SEND` + chooser）。真的弹出来了返回 true。 */
    fun share(text: String): Boolean

    /** 用浏览器打开网页（安卓是 Custom Tabs，保留返回栈）。打不开返回 false。 */
    fun openWeb(url: String): Boolean

    /** 用官方 App 的深链打开（`bilibili://video/…` 这种）。没有 App 能接这个地址时返回 false。 */
    fun openApp(deepLink: String): Boolean

    /** 桌面 / 演示 / 测试：什么都不接，调用方一律退化成复制。 */
    object None : SystemActions {
        override val available: Boolean get() = false
        override fun share(text: String): Boolean = false
        override fun openWeb(url: String): Boolean = false
        override fun openApp(deepLink: String): Boolean = false
    }
}
