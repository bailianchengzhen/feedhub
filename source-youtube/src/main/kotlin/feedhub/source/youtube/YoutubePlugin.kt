package feedhub.source.youtube

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
import feedhub.core.plugin.PluginTarget

/** YouTube 的平台标识。id 是 `yt`（不是 `youtube`），定了就别改。 */
internal val YOUTUBE: Platform = Platform.define("yt", "YouTube")

/**
 * YouTube 插件的描述（示范插件之二：境外、带额外依赖、桌面和安卓都有）。
 *
 * - **额外依赖**：n/sig 要跑 yt-dlp 的 EJS，用 quickjs-kt（`build.gradle.kts` 里的 `implementation(libs.quickjs.kt)`）。
 *   依赖跟着插件走：装了这个插件就带上 quickjs，拆掉就一起没了，主程序里没有任何"没有 quickjs 时降级"的代码。
 * - **桌面和安卓都跑**：quickjs-kt 1.0.5 是 KMP 库，安卓上解析成 `quickjs-kt-android`
 *   （AAR 里带 `arm64-v8a` / `x86_64` 的 `libquickjs.so`），n/sig 在模拟器上和 JVM 的答案逐字一致。
 *   以前这里写着"quickjs 只有 JVM 变体、安卓解不了 n/sig"——**那个判断是错的**，
 * 设备码登录 / 推荐流 / 订阅流 / 评论 / 搜索都是纯 OkHttp + JSON，
 *   安卓本来就能跑。
 * - **境外**：默认走代理、默认自动翻译。
 */
val YoutubePlugin: PlatformPlugin = PlatformPlugin(
    platform = YOUTUBE,
    color = 0xFFFF0000,
    // 接口 + 图片 + 视频 CDN（googlevideo.com 是分片 CDN；安卓上 Media3 的所有清单 / 分片请求都走
    // 这个平台的 OkHttp 客户端，代理和 fail-closed 都在里面）
    hosts = listOf("youtube.com", "ytimg.com", "ggpht.com", "googlevideo.com", "googleapis.com", "google.com"),
    capabilities = PlatformCapabilities(
        // 推荐（TV `browse` `default`）和订阅（`FEsubscriptions`）
        feeds = setOf(FeedKind.RECOMMEND, FeedKind.FOLLOWING),
        playableVideo = true,
        // TV browse 的 tile 全是视频：「只看图文」时不请求
        videoOnlyFeeds = setOf(FeedKind.RECOMMEND, FeedKind.FOLLOWING),
        // 搜索：`POST /youtubei/v1/search`（TVHTML5 7.x）。只有"相关"一档
        // （排序 / 时间筛选的 params 没有出处）；只收视频结果。
        search = SearchCapability(
            kinds = setOf(SearchKind.ALL, SearchKind.VIDEO),
            sorts = setOf(SearchSort.RELEVANCE),
            pageSize = 20,
        ),
    ),
    defaults = PlatformDefaults(feedOn = true, proxied = true, autoTranslate = true),
    login = PlatformLogin(
        // 登录是**设备码**（YoutubeSource 实现 DeviceCodeLoginSource），不是 Cookie：这里只给网页地址。
        // requiredCookies 空着，登录态以 ctx.store 里的 token 为准（whoami）。
        web = LoginSpec(
            webLoginUrl = "https://www.youtube.com/",
            cookieUrl = "https://www.youtube.com/",
            cookieDomain = ".youtube.com",
            requiredCookies = emptySet(),
        ),
        // 只给"导入 Cookie"这条备选路
        chromeImport = CookieImportSpec("youtube.com"),
    ),
    overseas = true,
    // 桌面 + 安卓。两边唯一的差别是 quickjs 的变体（由 Gradle 按平台挑，见 build.gradle.kts）；
    // 没有"哪个端用不了"的平台了，所以不再有 unavailableNote。
    targets = setOf(PluginTarget.DESKTOP, PluginTarget.ANDROID),
    // 推荐 / 订阅流的**整套请求逻辑在 Rust**：工厂直接建 Rust 版，没有开关。
    // 原来的 `YoutubeSource` 只留给请求等价 / 结果等价测试当参照（`RustYoutubeSource` 逐方法委托它）。
    factory = ::RustYoutubeSource,
)
