package feedhub.source.zhihu

import feedhub.core.FeedKind
import feedhub.core.ItemKind
import feedhub.core.LoginSpec
import feedhub.core.Platform
import feedhub.core.SearchCapability
import feedhub.core.SearchKind
import feedhub.core.SearchSort
import feedhub.core.plugin.AppLinks
import feedhub.core.plugin.CookieImportSpec
import feedhub.core.plugin.EmoteTable
import feedhub.core.plugin.PlatformCapabilities
import feedhub.core.plugin.PlatformDefaults
import feedhub.core.plugin.PlatformLogin
import feedhub.core.plugin.PlatformPlugin
import feedhub.core.plugin.PlatformTexts

/** 知乎的平台标识。id `zhihu` 定了就别改（存储键、cli 参数、条目 key 都用它）。 */
internal val ZHIHU: Platform = Platform.define("zhihu", "知乎")

/** 知乎插件的描述。构建期的那一半在模块根的 `plugin.toml`。 */
val ZhihuPlugin: PlatformPlugin = PlatformPlugin(
    platform = ZHIHU,
    color = 0xFF1772F6,
    // 接口、图片（pic1.zhimg.com 等）、视频（lens.zhihu.com）和专栏
    hosts = listOf("zhihu.com", "zhimg.com"),
    capabilities = PlatformCapabilities(
        feeds = setOf(FeedKind.RECOMMEND, FeedKind.FOLLOWING),
        playableVideo = true,
        // 搜索：`GET /api/v4/search_v3`，x-zse-96 现成。
        // 只收内容类结果（回答 / 文章 / 视频 / 想法 / 问题）；
        search = SearchCapability(
            kinds = setOf(SearchKind.ALL, SearchKind.VIDEO),
            // 在线实测：`sort=upvoted_count` 被服务端忽略（返回顺序和"相关"一致），
            // 所以只声明"相关"——不能让 --sort latest / hot 静默退化成默认排序。
            sorts = setOf(SearchSort.RELEVANCE),
            pageSize = 20,
        ),
    ),
    defaults = PlatformDefaults(feedOn = true),
    login = PlatformLogin(
        // 扫码登录（QrLoginSource）：账号页两端都显示「扫码登录」
        qrLogin = true,
        web = LoginSpec(
            webLoginUrl = "https://www.zhihu.com/signin",
            cookieUrl = "https://www.zhihu.com/",
            cookieDomain = ".zhihu.com",
            requiredCookies = setOf("z_c0", "d_c0"),
        ),
        chromeImport = CookieImportSpec("zhihu.com"),
    ),
    // 官方 App 的深链：知乎自己给的阅读历史里回答的地址就是 `zhihu://answers/<id>`（上游 zhihu-plus-plus
    // 的 `OnlineHistoryTest` 夹具）。问题 / 文章 / 作者主页没有实测过的格式，先只给网页链接（不猜）。
    appLinks = AppLinks(detail = mapOf(ItemKind.ANSWER to "zhihu://answers/{id}")),
    texts = PlatformTexts(videoPosterLabel = "作者", momentLabel = "想法"),
    emotes = EmoteTable(ZhihuSticker.PACK, ZhihuSticker.urls),
    // 推荐 / 关注动态的解析和 x-zse-96 签名用 Rust 实现。
    // HTTP / TLS / Cookie / 代理 / 限速还是同一套 OkHttp；原来的 Kotlin 解析（KotlinZhihuParse）只留给对拍当参照。
    factory = { ctx -> RustZhihuSource(ctx) },
)
