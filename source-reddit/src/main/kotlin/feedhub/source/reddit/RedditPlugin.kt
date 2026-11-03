package feedhub.source.reddit

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

/** Reddit 的平台标识。id `reddit` 定了就别改（存储键、cli 参数、条目 key 都用它）。 */
internal val REDDIT: Platform = Platform.define("reddit", "Reddit")

/** Reddit 插件的描述。构建期的那一半在模块根的 `plugin.toml`。 */
val RedditPlugin: PlatformPlugin = PlatformPlugin(
    platform = REDDIT,
    color = 0xFFFF4500,
    // 接口（www.reddit.com）、视频（v.redd.it）、图片 / 预览（i.redd.it、preview.redd.it、external-preview.redd.it
    // 都在 `redd.it` 下）、头像 / 缩略图（`redditmedia.com`、`redditstatic.com`）。
    // 正文里的站外链接（imgur、youtube……）**不算**（`Source.hosts` 的约定），那是复制链接 / 交给另一个平台打开的。
    hosts = listOf("reddit.com", "redd.it", "redditmedia.com", "redditstatic.com"),
    capabilities = PlatformCapabilities(
        feeds = setOf(FeedKind.RECOMMEND, FeedKind.FOLLOWING),
        // reddit_video 能播，但这一阶段没进「只看视频」名单（照原来的名单）
        // 搜索：`GET /search.json`，只收帖子（`type=link`）——所有结果本来就是 `t3`，
        // 所以 ALL / POST 都是同一份东西；
        // 排序：Reddit 搜索页自己的三档（`relevance` / `new` / `hot`）→ SearchSort 的三档，一一对应。
        // needsLogin = true：未登录的 `.json` 会被 403，那是**风控信号**——所以没登录时一个请求都不发
        // （和推荐流同一条规矩，见 RustRedditSource）。
        search = SearchCapability(
            kinds = setOf(SearchKind.ALL, SearchKind.POST),
            sorts = setOf(SearchSort.RELEVANCE, SearchSort.LATEST, SearchSort.HOT),
            pageSize = 50,
            needsLogin = true,
        ),
    ),
    defaults = PlatformDefaults(feedOn = true, proxied = true, autoTranslate = true),
    login = PlatformLogin(
        web = LoginSpec(
            webLoginUrl = "https://www.reddit.com/login/",
            cookieUrl = "https://www.reddit.com/",
            cookieDomain = ".reddit.com",
            requiredCookies = setOf("reddit_session"), // = RedditSource.SESSION_COOKIE,
        ),
        // 网页登录的 Cookie 在 .reddit.com（reddit_session 在 www.reddit.com），
        chromeImport = CookieImportSpec("reddit.com"),
    ),
    texts = PlatformTexts(communityFirst = true),
    overseas = true,
    // 工厂直接建 Rust 版（没有开关）：推荐流 / 关注流的"请求怎么发 + 怎么解析"都在 Rust，
    // 详情 / 评论 / 播放继续逐方法委托给 RedditSource（见 RustRedditSource）。
    factory = { ctx -> RustRedditSource(ctx) },
)
