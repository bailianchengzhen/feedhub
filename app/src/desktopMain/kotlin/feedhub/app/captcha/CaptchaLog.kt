package feedhub.app.captcha

import feedhub.captcha.RustCaptcha
import java.io.File

/**
 * 验证码相关的诊断日志：`~/.cache/feedhub/jcef/jcef.log`（和 CEF 原生包同一个目录，App 和验证码子进程都往里追加）。
 *
 * **只记状态和耗时**：不记 proof、Cookie，地址只记到路径（[redact] 去掉 query / fragment）。
 */
internal object CaptchaLog {
    fun installDir(): File {
        val xdg = System.getenv("XDG_CACHE_HOME")?.takeIf { it.isNotBlank() }
        val base = if (xdg != null) File(xdg) else File(System.getProperty("user.home"), ".cache")
        return File(base, "feedhub/jcef")
    }

    fun log(message: String) {
        runCatching {
            val f = File(installDir(), "jcef.log")
            if (f.length() > 2L * 1024 * 1024) f.delete()
            f.parentFile.mkdirs()
            f.appendText("${System.currentTimeMillis()} [${ProcessHandle.current().pid()}] $message\n")
        }
    }

    /** 地址只留到路径，去掉 query / hash。 */
    fun redact(url: String?): String = RustCaptcha.redactUrl(url)
}
