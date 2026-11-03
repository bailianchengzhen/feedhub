package feedhub.source.heybox

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

/** 小黑盒的平台标识。id `heybox` 定了就别改（存储键、cli 参数、条目 key 都用它）。 */
internal val HEYBOX: Platform = Platform.define("heybox", "小黑盒")

/** 小黑盒插件的描述。构建期的那一半在模块根的 `plugin.toml`。 */
val HeyboxPlugin: PlatformPlugin = PlatformPlugin(
    platform = HEYBOX,
    color = 0xFFE8463B,
    // 接口（api.xiaoheihe.cn）、图片 / 表情（imgheybox.max-c.com）、视频 CDN
    hosts = listOf("xiaoheihe.cn", "max-c.com"),
    capabilities = PlatformCapabilities(
        feeds = setOf(FeedKind.RECOMMEND),
        playableVideo = true,
        // 搜索：`GET /bbs/app/api/general/search/v1`，请求 `limit=20`。
        // - 只做**内容类**（`search_type=link`；`space` 用户卡 / `relevant_search` 相关搜索词丢掉）。
        // - 排序只有两档：相关（默认）和最新；
        //   `filter_list` 里还有"最多点赞 / 最多评论"，当前没验，不声明。
        // - pageSize 是**请求口径**（limit=20）；服务端实际回 30 条 link，游标按实际条数推进，
        search = SearchCapability(
            kinds = setOf(SearchKind.ALL),
            sorts = setOf(SearchSort.RELEVANCE, SearchSort.LATEST),
            pageSize = 20,
        ),
    ),
    defaults = PlatformDefaults(feedOn = true),
    login = PlatformLogin(
        // 扫码登录（QrLoginSource）：账号页两端都显示「扫码登录」
        qrLogin = true,
        web = LoginSpec(
            webLoginUrl = "https://www.xiaoheihe.cn/app/bbs/home",
            cookieUrl = "https://www.xiaoheihe.cn/",
            cookieDomain = ".xiaoheihe.cn",
            // xiaoheihe-cli auth.py finish_browser_login：pkey 和 heybox_id 都在才算登录了
            requiredCookies = setOf("pkey", "heybox_id"),
        ),
        chromeImport = CookieImportSpec("xiaoheihe.cn"),
    ),
    // 推荐流的解析和 hkey 签名用 Rust 实现。
    // HTTP 还是同一套 OkHttp；原来的 Kotlin 解析（KotlinHeyboxParse）只留给对拍当参照。
    factory = { ctx -> RustHeyboxSource(ctx) },
)
