package feedhub.app.login

import androidx.compose.runtime.Composable
import feedhub.core.LoginSpec
import feedhub.core.Platform
import feedhub.core.ProxyMode
import feedhub.core.ProxyType
import feedhub.core.parseCookieHeader
import okhttp3.Cookie

/**
 * 「网页登录」：打开平台官方的网页登录页，用户自己在页面里登录，页面把登录 Cookie 种进系统 WebView，
 * 我们在旁边看着**关键 Cookie** 出现，出现之后把它们拷进 [feedhub.core.FeedCookieJar]
 *
 * 安卓有实现（`AndroidWebLogin`），桌面当前没有（桌面走扫码 / 从 Chrome 导入 / 设备码）——`feedhub.app.AppGraph`
 * 里为 null，界面上就不显示"网页登录"按钮。
 *
 * **不代替用户登录**：不注入脚本、不自动填表、不点任何按钮；用户自己输账号密码 / 扫码。
 * [platform] 是 Compose 状态，界面读它决定要不要铺开登录页。
 */
interface WebLogin {
    /** 正在登录的平台；null = 没在登录。 */
    val platform: Platform?

    /** 打开这个平台的网页登录页。 */
    fun start(platform: Platform)

    /** 收起（用户取消，或登录完成、Cookie 已经拷好）。 */
    fun cancel()

    /**
     * 退出登录时清掉系统 WebView 里这个平台的 Cookie（只删 [LoginSpec.cookieDomain] 和它的子域，别的平台不碰）。
     *
     * 为什么要清：WebView 的 CookieManager 和我们的 Cookie 罐是两套。以前退出登录只清罐子，
     * WebView 里旧账号的 Cookie 还在，再点「网页登录」页面一秒就"登录成功"、把旧账号又写回罐子，换不了号。
     *
     * @param hosts 这个平台会用的域名（[feedhub.core.Source.hosts]）：`CookieManager` 没有"列出某个域所有 Cookie"
     *   的接口，只能用一组地址去问；除了登录页 / 取 Cookie 的地址 / 根域名，这些子域也问一遍
     *   （例如 `passport.bilibili.com`），否则只种在子域上的 Cookie 清不掉。
     *
     * 默认空实现（桌面没有 WebView 这一路）。
     */
    fun clearCookies(login: LoginSpec, hosts: List<String> = emptyList()) {}
}

/**
 * 打开 [login] 的网页登录页；关键 Cookie 全出现就调 [onDone]（参数是**已经只留这个平台域名**的 Cookie，
 * 调用方直接写进罐子），用户自己关掉就 [onCancel]。
 *
 * 桌面没有这个入口（actual 立刻 [onCancel]）。
 *
 * @param platform 只用来做 key / 显示（[login] 里已经有地址和关键 Cookie）。
 * @param proxy 打开页面前 WebView 这一侧要怎么走代理（[WebLoginProxyRules] 算出来的，和 OkHttp 同一份设置）。
 *   [WebLoginProxyPlan.Blocked] 时**一个字节都不加载**（境外平台 fail-closed）。
 */
@Composable
expect fun WebLoginScreen(
    platform: Platform,
    login: LoginSpec,
    proxy: WebLoginProxyPlan,
    onDone: (List<Cookie>) -> Unit,
    onCancel: () -> Unit,
)

/**
 * 登录 WebView 打开页面前的代理结论（纯逻辑，桌面 JVM 上有单测；安卓那边只负责把它交给 `ProxyController`）。
 *
 * 为什么单独一层：WebView 的代理和 OkHttp 是两套（`ProxyController` 只管 WebView 进程）。
 * 结论照样从 **App 给这个平台的那份代理**（`AppSettings.proxyFor`）来，不在这里列平台名单；
 * 境外平台（插件描述 `overseas`）只要不是"确实有代理"就 [Blocked]，宁可打不开也不直连。
 */
sealed interface WebLoginProxyPlan {
    /** 给 WebView 设这个代理。 */
    data class Override(val type: ProxyType, val host: String, val port: Int) : WebLoginProxyPlan {
        /** `ProxyConfig.Builder.addProxyRule` 认的写法（Chromium：`[scheme://]host[:port]`）。 */
        val rule: String
            get() = when (type) {
                ProxyType.SOCKS5 -> "socks5://$host:$port"
                ProxyType.HTTP -> "http://$host:$port"
            }
    }

    /** 这个平台不用代理（国内平台 + 设置里没让它走代理）：WebView 照设备自己的网络走。 */
    data object Direct : WebLoginProxyPlan

    /** 该走代理但走不了（或境外平台直连）：不加载页面，只给用户这句话。 */
    data class Blocked(val message: String) : WebLoginProxyPlan
}

/** [WebLoginProxyPlan] 的唯一判定处（`WebLoginProxyTest` 离线测）。 */
object WebLoginProxyRules {
    /**
     * @param mode App 现在给这个平台的代理（[feedhub.core.ProxyProvider] 的结论，和接口 / 图片同一份）。
     * @param overseas 插件描述的 `overseas`（不按平台名判断）。
     */
    fun plan(mode: ProxyMode, overseas: Boolean): WebLoginProxyPlan = when (mode) {
        is ProxyMode.Custom -> if (mode.valid) {
            WebLoginProxyPlan.Override(mode.type, mode.host, mode.port)
        } else {
            WebLoginProxyPlan.Blocked("代理没就绪：「指定代理」的地址或端口没填全。先在「设置 → 网络」里填好再来登录。")
        }
        is ProxyMode.BuiltinCore -> if (mode.valid) {
            WebLoginProxyPlan.Override(ProxyType.SOCKS5, "127.0.0.1", mode.port)
        } else {
            WebLoginProxyPlan.Blocked(
                "代理没就绪：内置代理核心没在运行" + (mode.reason?.takeIf { it.isNotBlank() }?.let { "（$it）" } ?: "") +
                    "。先在「设置 → 进阶设置 → 代理（进阶）」里让它跑起来，再来登录。",
            )
        }
        ProxyMode.Direct -> if (overseas) {
            // 境外平台的登录页也必须走代理（红线）；出口锁关着时 OkHttp 会直连，但 WebView 这里不跟。
            WebLoginProxyPlan.Blocked("这是境外平台，但当前设置让它直连。境外平台不允许直连，先到「设置 → 网络」里把它勾成走代理。")
        } else {
            WebLoginProxyPlan.Direct
        }
        ProxyMode.System -> if (overseas) {
            // WebView 拿不到 JVM 的「系统代理」（那是 java.net 的设置）；不能在不知道出口的情况下去连境外平台。
            WebLoginProxyPlan.Blocked(
                "现在用的是「系统代理」，WebView 读不到 JVM 的系统代理设置。请在「设置 → 网络」里选「指定」（SOCKS5 / HTTP）" +
                    "或「内置核心」，再来登录境外平台。",
            )
        } else {
            WebLoginProxyPlan.Direct
        }
    }
}

/**
 * 网页登录里"判定 / 拷贝 / 清理"这几步的纯逻辑：喂它假的 Cookie 字符串就能测（`WebLoginCookiesTest`），
 * 不用真开 WebView。安卓那边的 actual 只负责"问 CookieManager 要字符串"和画 WebView。
 */
object WebLoginCookies {
    /**
     * 从 WebView 的 CookieManager 拿到的几段 `name=value; name2=value2` 里解析出 Cookie，统一记到 [domain]。
     *
     * 为什么要"统一记域"：`android.webkit.CookieManager.getCookie(url)` 只给名字和值，
     * 没有域 / 路径 / 过期时间；平台的 [LoginSpec.cookieDomain] 是我们知道的域（`.bilibili.com`），
     * 过期给一年（和 [parseCookieHeader] 一样，真失效与否以服务端为准）。同名只留第一个。
     */
    fun parse(headers: List<String>, domain: String): List<Cookie> =
        headers.filter { it.isNotBlank() }.flatMap { parseCookieHeader(it, domain) }.distinctBy { it.name }

    /** 登录完成了吗：关键 Cookie（[LoginSpec.requiredCookies]）一个不少。空的关键 Cookie = 认不出来，永远算没完成。 */
    fun isComplete(required: Set<String>, cookies: List<Cookie>): Boolean =
        required.isNotEmpty() && required.all { name -> cookies.any { it.name == name } }

    /** 关键 Cookie 现在各是什么值（只拿来和页面刚打开时比"变没变"，不打印、不落盘）。 */
    fun values(required: Set<String>, cookies: List<Cookie>): Map<String, String> =
        required.mapNotNull { name -> cookies.firstOrNull { it.name == name }?.let { name to it.value } }.toMap()

    /**
     * 这次是不是**真的登录了**：关键 Cookie 齐了，而且和页面刚打开时（[baseline]）**至少有一个不一样**。
     *
     * 为什么要比 baseline：小红书的 `web_session` 在**游客态**就会种。只看"齐没齐"
     * 会在页面刚打开时就误判成登录完成、把游客 Cookie 写进罐子并把登录页关掉——用户根本没机会登录。
     * 游客 Cookie 的值在真登录后会换（X 的 `auth_token`、B站 `SESSDATA` 这类则是登录后才出现），
     * 所以"齐了 + 变了"才算。**已经登录过、WebView 里本来就有全套 Cookie** 时这里不会自动完成，
     * 用户可以点登录页右上角的「完成登录」直接保存（见 `WebLoginScreen`）。
     */
    fun isNewLogin(required: Set<String>, baseline: Map<String, String>, cookies: List<Cookie>): Boolean {
        if (!isComplete(required, cookies)) return false
        val current = values(required, cookies)
        return current.any { (name, value) -> baseline[name] != value }
    }

    /** 要写进罐子的那一份：只留这个平台域名（含子域）下的，别的域名不碰。 */
    fun toStore(cookieDomain: String, cookies: List<Cookie>): List<Cookie> {
        val d = cookieDomain.trimStart('.').lowercase()
        if (d.isEmpty()) return emptyList()
        return cookies.filter { c ->
            val cd = c.domain.trimStart('.').lowercase()
            cd == d || cd.endsWith(".$d")
        }
    }

    /**
     * 要问 CookieManager 哪几个地址：`CookieManager.getCookie(url)` 返回的是"这个地址当时会带上的 Cookie"，
     * 所以要把登录页、取 Cookie 的地址和根域名都问一遍（有的平台关键 Cookie 只在某个子域下）。
     * 顺序固定、去重，便于测试。
     */
    fun probeUrls(login: LoginSpec): List<String> {
        val root = login.cookieDomain.trimStart('.')
        return listOfNotNull(
            login.webLoginUrl.takeIf { it.isNotBlank() },
            login.cookieUrl.takeIf { it.isNotBlank() },
            "https://$root/".takeIf { root.isNotBlank() },
        ).distinct()
    }

    /** [host]（或一条 Cookie 的域）是不是 [cookieDomain] 本身或它的子域。 */
    fun domainMatches(cookieDomain: String, host: String): Boolean {
        val d = cookieDomain.trimStart('.').lowercase()
        val h = host.trimStart('.').lowercase().substringBefore(':')
        if (d.isEmpty() || h.isEmpty()) return false
        return h == d || h.endsWith(".$d")
    }

    /**
     * `androidx.webkit.CookieManagerCompat.getCookieInfo` 的一条（Chromium 给的是 **Set-Cookie 形状**，
     * 带 `Domain=` / `Path=`）里我们需要的三样。老设备拿不到这些属性时走 [fallbackDeletionHeaders]。
     */
    fun parseInfo(line: String): WebCookieRef? {
        val parts = line.split(';').map { it.trim() }.filter { it.isNotEmpty() }
        val first = parts.firstOrNull() ?: return null
        val name = first.substringBefore('=').trim()
        if (name.isEmpty()) return null
        var domain: String? = null
        var path = "/"
        for (p in parts.drop(1)) {
            val key = p.substringBefore('=', "").trim().lowercase()
            val value = p.substringAfter('=', "").trim()
            when (key) {
                "domain" -> if (value.isNotEmpty()) domain = value
                "path" -> if (value.isNotEmpty() && value.startsWith("/")) path = value
            }
        }
        return WebCookieRef(name, domain, path)
    }

    /** 删除一条 Cookie 用的 Set-Cookie：同名、同域、同路径、立刻过期（值留空）。 */
    fun deletionHeader(ref: WebCookieRef): String = buildString {
        append(ref.name).append("=; Max-Age=0; Expires=Thu, 01 Jan 1970 00:00:00 GMT; Path=").append(ref.path)
        ref.domain?.takeIf { it.isNotBlank() }?.let { append("; Domain=").append(it) }
    }

    /**
     * 设备 WebView 不支持 `getCookieInfo`（拿不到域 / 路径）时的兜底：按探测地址删两次——
     * host-only 的那条 + 整域（[cookieDomain]）的那条。路径只知道 `/`，别的路径上的同名 Cookie 删不掉（写在结果里）。
     */
    fun fallbackDeletionHeaders(name: String, cookieDomain: String): List<String> {
        val domain = cookieDomain.trimStart('.')
        return listOf(
            "$name=; Max-Age=0; Expires=Thu, 01 Jan 1970 00:00:00 GMT; Path=/",
            "$name=; Max-Age=0; Expires=Thu, 01 Jan 1970 00:00:00 GMT; Path=/; Domain=$domain",
        ).distinct()
    }
}

/** 一条 Cookie 的身份（名字 + 域 + 路径）：删除时这三个要跟原来一样。 */
data class WebCookieRef(val name: String, val domain: String?, val path: String)
