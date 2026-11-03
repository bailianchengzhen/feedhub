package feedhub.app.captcha

import feedhub.captcha.RustCaptcha

/**
 * 验证码浏览器（JCEF）的导航白名单。
 *
 * 页面本身是本机临时服务上的验证页，引的是网易易盾的远程脚本（`https://cstaticdun.126.net/load.min.js`）：
 * - **主框架导航**只允许本机验证页：`http://127.0.0.1:<本次端口>/…`；
 * - **子框架（iframe）导航**只允许易盾自家的域名：`*.126.net` / `*.163.com` / `*.163yun.com`；
 * - `about:blank` / `about:srcdoc` 放行；
 * - **所有弹窗拦掉**。
 *
 * 核心判断逻辑在 Rust（`RustCaptcha`），此处为宿主门面。
 */
internal class CaptchaNavigationPolicy(private val port: Int) {

    /** 主框架：只准本机验证页（http + 127.0.0.1 + 本次端口，路径不限）。 */
    fun isMainFrameAllowed(url: String): Boolean =
        RustCaptcha.isMainFrameAllowed(port, url)

    /** 子框架：易盾的域名 + about:blank。其余一律不放行。 */
    fun isSubFrameAllowed(url: String): Boolean =
        RustCaptcha.isSubFrameAllowed(url)

    companion object {
        /** 从页面地址里取本次验证页的端口；取不到给 -1（那就什么都不放行）。 */
        fun portOf(url: String): Int = RustCaptcha.portOf(url)
    }
}

/** 够用的 URL 拆解结果。 */
internal data class ParsedUrl(val scheme: String, val host: String, val port: Int) {
    /** 没写端口时按协议默认端口（http 80 / https 443）。 */
    val effectivePort: Int get() = when {
        port >= 0 -> port
        scheme == "http" -> 80
        scheme == "https" -> 443
        else -> -1
    }
}

internal fun parseUrl(raw: String): ParsedUrl? {
    val ffi = RustCaptcha.parseUrl(raw) ?: return null
    return ParsedUrl(scheme = ffi.scheme, host = ffi.host, port = ffi.port)
}
