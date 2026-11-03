package feedhub.app.login

import feedhub.core.Platform

/**
 * 从本机浏览器导入登录 Cookie。
 * 主要给不能扫码的酷安用，其他平台也可以用。安卓没有这个（`AppGraph.cookieImporter` 为 null）。
 */
interface CookieImporter {
    /** 按钮上显示的浏览器名，例如 "Chrome"。 */
    val browserName: String

    /** 这个平台能不能从浏览器导入（插件描述里有没有 `login.chromeImport`）。不能的平台不显示导入按钮。 */
    fun supports(platform: Platform): Boolean = true

    /**
     * 导入这些平台的 Cookie，**替换**本机原有的（只动这几个平台的域名，其他不碰）。
     * 读不到浏览器、取不到密钥之类的整体失败抛 [CookieImportException]。只返回数量，从不返回 Cookie 的值。
     */
    suspend fun import(platforms: Set<Platform>): Map<Platform, ImportResult>
}

data class ImportResult(val imported: Int, val failed: Int)

class CookieImportException(message: String) : Exception(message)
