package feedhub.app.login

import android.webkit.CookieManager
import androidx.webkit.CookieManagerCompat
import androidx.webkit.WebViewFeature
import feedhub.core.LoginSpec
import okhttp3.HttpUrl.Companion.toHttpUrl

/**
 * 安卓：清掉系统 WebView 里**某一个平台**的 Cookie（退出登录用）。
 *
 * 为什么要专门做：WebView 的 `CookieManager` 和我们的 Cookie 罐是两套存储。平台登录页把登录 Cookie 种进
 * WebView，我们再拷一份进罐子；退出登录如果只清罐子，WebView 里旧账号的 Cookie 还在，再点「网页登录」
 * 页面会立刻"登录成功"、把旧号又写回罐子 —— 换不了号。
 *
 * 怎么只删一个平台：
 * - API / WebView 支持 `GET_COOKIE_INFO`（Chromium 105+）时用 `CookieManagerCompat.getCookieInfo(url)`：
 *   它给的是 **Set-Cookie 形状**（带 `Domain=` / `Path=`），按原来的域和路径写一条立刻过期的同名 Cookie
 *   就精确删掉了；删除前按 [LoginSpec.cookieDomain]（含子域）过滤，别的平台的 Cookie 一条都不碰。
 * - 老 WebView 拿不到属性：退化成"按探测地址的 Cookie 名，host-only + 整域各删一次"。
 *
 * 只在主线程调（`CookieManager` 的写操作走 WebView 的 IO 线程，但别在别的线程和 WebView 的加载抢）。
 * 不打印任何 Cookie 名字 / 值，只返回删了几条（给 logcat 一行数量）。
 */
internal object AndroidWebCookies {
    /** @return 写了几条删除 Cookie（`CookieManager.setCookie` 是 void，没法回读，所以是"尝试删除"的条数）。 */
    fun clear(login: LoginSpec, hosts: List<String> = emptyList()): Int {
        val cm = CookieManager.getInstance()
        var deleted = 0
        // 登录页 / 取 Cookie 的地址 / 根域名 + 这个平台会用到的子域（Source.hosts）：
        // CookieManager 不能枚举，只能逐个地址问
        val probes = (WebLoginCookies.probeUrls(login) + hosts.map { "https://${it.trimStart('.')}/" }).distinct()

        if (WebViewFeature.isFeatureSupported(WebViewFeature.GET_COOKIE_INFO)) {
            // key → (发请求用的地址, Cookie 身份)
            val found = LinkedHashMap<String, Pair<String, WebCookieRef>>()
            fun collect(url: String) {
                val lines = runCatching { CookieManagerCompat.getCookieInfo(cm, url) }.getOrNull().orEmpty()
                for (line in lines) {
                    val ref = WebLoginCookies.parseInfo(line) ?: continue
                    val host = ref.domain?.trimStart('.')
                        ?: runCatching { url.toHttpUrl().host }.getOrNull()
                        ?: continue
                    // 只删这个平台的（域名本身或子域），别的平台不动
                    if (!WebLoginCookies.domainMatches(login.cookieDomain, host)) continue
                    found.putIfAbsent("${ref.name}|${ref.domain}|${ref.path}|$host", url to ref)
                }
            }
            probes.forEach(::collect)
            // 登录 Cookie 有的只在某个子域下（passport.bilibili.com 之类）：把发现的域再问一轮根地址
            val extra = found.values.mapNotNull { it.second.domain }
                .filter { WebLoginCookies.domainMatches(login.cookieDomain, it) }
                .map { "https://${it.trimStart('.')}/" }
                .distinct()
                .filterNot { it in probes }
            extra.forEach(::collect)
            for ((url, ref) in found.values) {
                runCatching { cm.setCookie(url, WebLoginCookies.deletionHeader(ref)) }.onSuccess { deleted++ }
            }
        } else {
            // 老设备：只有 name=value，没有域 / 路径。按探测地址把同名 Cookie 删两次（host-only + 整域）。
            for (url in probes) {
                val header = runCatching { cm.getCookie(url) }.getOrNull().orEmpty()
                if (header.isBlank()) continue
                for (part in header.split(';')) {
                    val name = part.trim().substringBefore('=').trim()
                    if (name.isEmpty()) continue
                    for (set in WebLoginCookies.fallbackDeletionHeaders(name, login.cookieDomain)) {
                        runCatching { cm.setCookie(url, set) }.onSuccess { deleted++ }
                    }
                }
            }
        }
        runCatching { cm.flush() }
        return deleted
    }
}
