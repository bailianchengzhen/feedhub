package feedhub.source.xiaohongshu

import feedhub.core.FeedKind
import feedhub.core.LoginSpec
import feedhub.core.Platform
import feedhub.core.SearchCapability
import feedhub.core.SearchKind
import feedhub.core.SearchSort
import feedhub.core.plugin.CookieImportSpec
import feedhub.core.plugin.PlatformCapabilities
import feedhub.core.plugin.PlatformDefaults
import feedhub.core.plugin.PlatformLogin
import feedhub.core.plugin.PlatformPlugin
import feedhub.core.plugin.PlatformTexts

/** 小红书的平台标识。id `xhs` 定了就别改（存储键、cli 参数、条目 key 都用它）。 */
internal val XHS: Platform = Platform.define("xhs", "小红书")

/** 小红书插件的描述。构建期的那一半在模块根的 `plugin.toml`。 */
val XhsPlugin: PlatformPlugin = PlatformPlugin(
    platform = XHS,
    color = 0xFFFF2442,
    // 接口（edith.xiaohongshu.com）、图片 / 表情（sns-img-*.xhscdn.com、picasso-static）、视频 CDN、分享短链
    hosts = listOf("xiaohongshu.com", "xhscdn.com", "xhslink.com"),
    capabilities = PlatformCapabilities(
        feeds = setOf(FeedKind.RECOMMEND, FeedKind.FOLLOWING),
        // 评论接口没有排序参数（只有一种顺序）
        commentSort = false,
        playableVideo = true,
        // 搜索：`POST /api/sns/web/v1/search/notes`（v1 / edith，`x-rap-param` 现成）。
        // 只做「综合」（`sort=general`）、只收内容类结果（`note_type=0`，视频 / 图文都收）；
        // 不登录搜不了（没有 web_session 所有业务接口都回 -101），而且搜索是 -104 / 461 / 471 的高发接口。
        search = SearchCapability(
            kinds = setOf(SearchKind.ALL),
            sorts = setOf(SearchSort.RELEVANCE),
            pageSize = XhsSearch.PAGE_SIZE,
            needsLogin = true,
        ),
    ),
    defaults = PlatformDefaults(feedOn = true),
    login = PlatformLogin(
        // 扫码登录（QrLoginSource）：账号页两端都显示「扫码登录」
        qrLogin = true,
        web = LoginSpec(
            webLoginUrl = "https://www.xiaohongshu.com/explore",
            cookieUrl = XhsApi.WEB_URL,
            cookieDomain = ".xiaohongshu.com",
            // 参考项目都校验 a1 + web_session：只有 a1 没有 web_session 时业务接口回 -101
            requiredCookies = setOf("a1", "web_session"),
        ),
        chromeImport = CookieImportSpec("xiaohongshu.com"),
    ),
    texts = PlatformTexts(posterLabel = "作者"),
    // 推荐流 / 关注流三个接口的解析和请求签名用 Rust 实现。
    // HTTP / TLS / Cookie / 代理 / 限速还是同一套 OkHttp；原来的 Kotlin 解析（KotlinXhsParse）只留给对拍当参照。
    factory = { ctx -> RustXhsSource(ctx) },
)
