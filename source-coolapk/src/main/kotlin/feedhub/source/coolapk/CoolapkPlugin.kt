package feedhub.source.coolapk

import feedhub.core.FeedKind
import feedhub.core.LoginSpec
import feedhub.core.Platform
import feedhub.core.SearchCapability
import feedhub.core.SearchKind
import feedhub.core.SearchSort
import feedhub.core.plugin.CaptchaPage
import feedhub.core.plugin.CookieImportSpec
import feedhub.core.plugin.EmoteTable
import feedhub.core.plugin.PlatformCapabilities
import feedhub.core.plugin.PlatformDefaults
import feedhub.core.plugin.PlatformLogin
import feedhub.core.plugin.PlatformPlugin

/** 酷安的平台标识。id `coolapk` 定了就别改（存储键、cli 参数、条目 key 都用它）。 */
internal val COOLAPK: Platform = Platform.define("coolapk", "酷安")

/** 酷安插件的描述。构建期的那一半在模块根的 `plugin.toml`。 */
val CoolapkPlugin: PlatformPlugin = PlatformPlugin(
    platform = COOLAPK,
    color = 0xFF11A562,
    // 接口（api / api2）、账号页、图片（image.coolapk.com 等）、酷安托管的视频（video-qc）
    hosts = listOf("coolapk.com"),
    capabilities = PlatformCapabilities(
        feeds = setOf(FeedKind.RECOMMEND, FeedKind.FOLLOWING),
        // 托管视频有防盗链签名，放不了：不进「只看视频」
        // 搜索：`GET /v6/search`，和信息流同一套身份（13.4.1 / API 13 / v2 token）。
        // 排序四档（none / dateline / hot / reply）；可能一搜就弹 err_request_captcha*，按风控停。
        search = SearchCapability(
            kinds = setOf(SearchKind.ALL, SearchKind.VIDEO, SearchKind.POST),
            sorts = setOf(SearchSort.RELEVANCE, SearchSort.LATEST, SearchSort.HOT),
            pageSize = 20,
        ),
    ),
    defaults = PlatformDefaults(feedOn = true),
    login = PlatformLogin(
        web = LoginSpec(
            // Coolapk-UWP 的网页登录入口（UriHelper.LoginUri），登录成功后跳回 www.coolapk.com，再从 Cookie 里取 uid/username/token
            webLoginUrl = "https://account.coolapk.com/auth/loginByCoolapk",
            cookieUrl = "https://www.coolapk.com/",
            cookieDomain = ".coolapk.com",
            requiredCookies = setOf("uid", "username", "token"),
        ),
        chromeImport = CookieImportSpec("coolapk.com"),
        noQrHint = "用 feedhub cookie import coolapk 粘贴 Cookie",
    ),
    // 键是正文里的原文：方括号 `[xxx]`，圆括号要带 `#`（`#(xxx)`），见 CoolapkEmoji
    emotes = EmoteTable(
        CoolapkEmoji.PACK,
        CoolapkEmoji.table.keys.associate { code -> (if (code.startsWith("(")) "#$code" else code).let { it to CoolapkEmoji.url(it)!! } },
        CoolapkEmoji::spans,
    ),
    captchaPage = CaptchaPage(CoolapkCaptchaPage::html),
    // 推荐 / 关注流的解析和 X-App-Token（v2 / v3）用 Rust 实现。HTTP / TLS / Cookie / 代理 / 限速还是同一套 OkHttp；原来的 Kotlin 解析（KotlinCoolapkParse）只留给
    // 对拍当参照。酷安多两个可注入的地址参数（测试用），所以写不成构造引用。
    factory = { ctx -> RustCoolapkSource(ctx) },
)
