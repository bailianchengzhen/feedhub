package feedhub.source.bilibili

import feedhub.core.FeedKind
import feedhub.core.ItemKind
import feedhub.core.LoginSpec
import feedhub.core.Platform
import feedhub.core.SearchCapability
import feedhub.core.SearchKind
import feedhub.core.SearchSort
import feedhub.core.plugin.AppLinks
import feedhub.core.plugin.CookieImportSpec
import feedhub.core.plugin.PlatformCapabilities
import feedhub.core.plugin.PlatformDefaults
import feedhub.core.plugin.PlatformLogin
import feedhub.core.plugin.PlatformPlugin
import feedhub.core.plugin.PlatformTexts

/** B站的平台标识。id `bili` 定了就别改（存储键、cli 参数、条目 key 都用它）。 */
internal val BILIBILI: Platform = Platform.define("bili", "B站")

/**
 * B站插件的描述（示范插件之一：功能最全——弹幕、楼中楼、相关推荐、关注管理、扫码登录）。
 * 构建期的那一半（id、描述在哪）在模块根的 `plugin.toml`。
 */
val BilibiliPlugin: PlatformPlugin = PlatformPlugin(
    platform = BILIBILI,
    color = 0xFFFB7299,
    // 接口、图片（i0.hdslb.com 等）、视频 CDN（bilivideo.com 等）和分享短链
    hosts = listOf("bilibili.com", "hdslb.com", "biliimg.com", "bilivideo.com", "bilivideo.cn", "b23.tv"),
    capabilities = PlatformCapabilities(
        feeds = setOf(FeedKind.RECOMMEND, FeedKind.FOLLOWING),
        danmaku = true, // `/x/v2/dm/wbi/web/seg.so`，每段 6 分钟
        relatedVideos = true, // 视频详情的 Related
        playableVideo = true,
        // 推荐流全是视频；关注流（动态 / 图文）还要
        videoOnlyFeeds = setOf(FeedKind.RECOMMEND),
        // 搜索：`/x/web-interface/wbi/search/type?search_type=video`，WBI 签名现成。
        // page_size 请求 30，响应可能只回 20。
        search = SearchCapability(
            kinds = setOf(SearchKind.ALL, SearchKind.VIDEO),
            sorts = setOf(SearchSort.RELEVANCE, SearchSort.LATEST, SearchSort.HOT),
            pageSize = 30,
        ),
    ),
    defaults = PlatformDefaults(feedOn = true),
    login = PlatformLogin(
        // 扫码登录（QrLoginSource）：账号页两端都显示「扫码登录」
        qrLogin = true,
        web = LoginSpec(
            webLoginUrl = "https://passport.bilibili.com/login",
            cookieUrl = "https://www.bilibili.com/",
            cookieDomain = ".bilibili.com",
            requiredCookies = setOf("SESSDATA", "bili_jct", "DedeUserID"),
        ),
        chromeImport = CookieImportSpec("bilibili.com"),
    ),
    // 官方 App 的深链。
    // 视频 bvid / avid 都认；动态用 id_str；直播用房间号；作者主页用 mid。
    appLinks = AppLinks(
        detail = mapOf(
            ItemKind.VIDEO to "bilibili://video/{id}",
            ItemKind.LIVE to "bilibili://live/{id}",
            ItemKind.MOMENT to "bilibili://following/detail/{id}",
        ),
        user = "bilibili://space/{userId}",
    ),
    texts = PlatformTexts(
        videoPosterLabel = "UP主",
        followTab = "B站 UP 主",
        followNoun = " UP 主",
    ),
    // 推荐 / 关注的解析和 WBI 签名用 Rust 实现。
    // HTTP 还是同一套 OkHttp；原来的 Kotlin 解析（KotlinBiliParse）只留给对拍当参照。
    factory = { ctx -> RustBiliSource(ctx) },
)
