package feedhub.source.tieba

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

/** 贴吧的平台标识。id `tieba` 定了就别改（存储键、cli 参数、条目 key 都用它）。 */
internal val TIEBA: Platform = Platform.define("tieba", "贴吧")

/** 贴吧插件的描述。构建期的那一半在模块根的 `plugin.toml`。 */
val TiebaPlugin: PlatformPlugin = PlatformPlugin(
    platform = TIEBA,
    color = 0xFF4E6EF2,
    // 接口（App / 网页 / 通行证）、图片（tb1.bdstatic.com、himg.bdimg.com 等）
    hosts = listOf("baidu.com", "bdstatic.com", "bdimg.com", "tieba.com"),
    capabilities = PlatformCapabilities(
        feeds = setOf(FeedKind.RECOMMEND, FeedKind.FOLLOWING),
        playableVideo = true,
        // 搜索：默认全吧搜索（网页接口 + PC salt）；清吧里给 `community` 走吧内 App 协议。
        // 排序只有"最相关 / 最新"（`st`：2 / 5），没有"最热"。
        search = SearchCapability(
            kinds = setOf(SearchKind.ALL, SearchKind.POST),
            sorts = setOf(SearchSort.RELEVANCE, SearchSort.LATEST),
            pageSize = 30,
        ),
    ),
    defaults = PlatformDefaults(feedOn = true),
    login = PlatformLogin(
        // 扫码登录（QrLoginSource）：账号页两端都显示「扫码登录」
        qrLogin = true,
        web = LoginSpec(
            // TiebaLite `ui/page/login/LoginPage.kt` 的 LOGIN_URL
            webLoginUrl = "https://wappass.baidu.com/passport?login&u=https%3A%2F%2Ftieba.baidu.com%2Findex%2Ftbwise%2Fmine",
            cookieUrl = "https://tieba.baidu.com/",
            cookieDomain = ".baidu.com",
            requiredCookies = setOf("BDUSS", "STOKEN"),
        ),
        // 百度只取 `.baidu.com` 和 `tieba.baidu.com`，网盘等其他子域不碰
        chromeImport = CookieImportSpec(
            domain = "baidu.com",
            suffixes = emptyList(),
            exactHosts = setOf(".baidu.com", "baidu.com", ".tieba.baidu.com", "tieba.baidu.com"),
        ),
    ),
    texts = PlatformTexts(communitySuffix = "吧"),
    // 请求、签名、解析都在 Rust（没有开关）；Kotlin 原来那份实现只在测试里当对拍参照。
    factory = { ctx -> RustTiebaSource(ctx) },
)
