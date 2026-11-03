package feedhub.source.x

import feedhub.core.FeedKind
import feedhub.core.LoginSpec
import feedhub.core.Platform
import feedhub.core.plugin.CookieImportSpec
import feedhub.core.plugin.PlatformCapabilities
import feedhub.core.plugin.PlatformDefaults
import feedhub.core.plugin.PlatformLogin
import feedhub.core.plugin.PlatformPlugin

/** X 的平台标识。id `x` 定了就别改（存储键、cli 参数、条目 key 都用它）。 */
internal val X: Platform = Platform.define("x", "X")

/** X 插件的描述。构建期的那一半在模块根的 `plugin.toml`。 */
val XPlugin: PlatformPlugin = PlatformPlugin(
    platform = X,
    color = 0xFF0F1419,
    // 接口（x.com）、分享短链、脚本 / 图片 / 视频 CDN（twimg.com）、老域（twitter.com）。
    hosts = listOf("x.com", "twitter.com", "twimg.com"),
    capabilities = PlatformCapabilities(
        feeds = setOf(FeedKind.RECOMMEND, FeedKind.FOLLOWING),
        playableVideo = true,
    ),
    defaults = PlatformDefaults(feedOn = true, proxied = true, autoTranslate = true),
    login = PlatformLogin(
        web = LoginSpec(
            webLoginUrl = "https://x.com/i/flow/login",
            cookieUrl = "https://x.com/", // = XSource.COOKIE_URL
            cookieDomain = ".x.com",
            requiredCookies = setOf("auth_token", "ct0"),
        ),
        // 登录 Cookie 在 .x.com；老域 .twitter.com 里的也一起收
        chromeImport = CookieImportSpec("x.com", suffixes = listOf("x.com", "twitter.com")),
    ),
    overseas = true,
    // 没有开关：推荐流 / 关注流整套请求在 Rust，
    // 详情 / 评论 / 播放那些逐方法委托 Kotlin 的 XSource。见 RustXSource。
    factory = ::RustXSource,
)
