package feedhub.app

import java.util.concurrent.TimeUnit

/**
 * 桌面版的界面缩放。KDE Wayland 下 Java（走 XWayland）读不到系统缩放，4K 屏上界面会按 1 倍画、字小得没法看，
 * 所以自己读系统设置，启动时设给 Java（`sun.java2d.uiScale`，Linux 上只认整数），小数部分在 Compose 里用 LocalDensity 补上。
 * 设置里"界面缩放"不是自动时以设置为准。
 */
object UiScale {
    /** 系统的缩放倍数，读不到就 null。依次看：GDK_SCALE、XWayland 的 Xft.dpi（÷96）、KWin 的 Xwayland 缩放设置。 */
    val system: Float? by lazy {
        System.getenv("GDK_SCALE")?.toFloatOrNull()?.takeIf { it > 0f }
            ?: run("xrdb", "-query")?.lineSequence()
                ?.firstOrNull { it.startsWith("Xft.dpi") }?.substringAfter(':')?.trim()?.toFloatOrNull()
                ?.takeIf { it > 0f }?.let { it / 96f }
            ?: run("kreadconfig6", "--file", "kwinrc", "--group", "Xwayland", "--key", "Scale")?.toFloatOrNull()?.takeIf { it > 0f }
    }

    /** 这次要用的倍数：设置里选了就用设置的（[AppSettings.uiScale] > 0），否则跟系统，都没有就 1。 */
    fun wanted(settings: AppSettings): Float = settings.uiScale.takeIf { it > 0f } ?: system ?: 1f

    /** 启动时、碰 AWT 之前调：把整数部分交给 Java（窗口大小、标题栏跟着变），返回给到 Java 的倍数。 */
    fun applyToJava(scale: Float): Float {
        val whole = scale.toInt().coerceAtLeast(1)
        if (System.getProperty("sun.java2d.uiScale") == null && whole > 1) {
            System.setProperty("sun.java2d.uiScale.enabled", "true")
            System.setProperty("sun.java2d.uiScale", whole.toString())
        }
        return whole.toFloat()
    }

    private fun run(vararg cmd: String): String? = try {
        val p = ProcessBuilder(*cmd).redirectError(ProcessBuilder.Redirect.DISCARD).start()
        val out = p.inputStream.bufferedReader().readText()
        if (p.waitFor(3, TimeUnit.SECONDS) && p.exitValue() == 0) out.trim().ifEmpty { null } else null
    } catch (_: Exception) {
        null
    }
}
