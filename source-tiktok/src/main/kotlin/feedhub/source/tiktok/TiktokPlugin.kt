package feedhub.source.tiktok

import feedhub.core.FeedKind
import feedhub.core.LoginSpec
import feedhub.core.Platform
import feedhub.core.plugin.CookieImportSpec
import feedhub.core.plugin.PlatformCapabilities
import feedhub.core.plugin.PlatformDefaults
import feedhub.core.plugin.PlatformLogin
import feedhub.core.plugin.PlatformPlugin

/** TikTok 的平台标识。id `tiktok` 定了就别改（存储键、cli 参数、条目 key 都用它）。 */
internal val TIKTOK: Platform = Platform.define("tiktok", "TikTok")

/** TikTok 插件的描述。构建期的那一半在模块根的 `plugin.toml`。 */
val TiktokPlugin: PlatformPlugin = PlatformPlugin(
    platform = TIKTOK,
    color = 0xFF25F4EE,
    // 真的会去连的域名：接口 / 视频页在 `tiktok.com`，视频与封面在 `tiktokcdn.com` /
    // `tiktokcdn-us.com`（f2 抓的 SDK 域名是 `lf16-tiktok-web.tiktokcdn-us.com`，web 的
    // `playAddr` 常见 `v16-webapp-prime.tiktok.com` / `v16m.tiktokcdn.com`），图片还有 `ibytedtos.com`。
    // TikTok 是境外平台，默认点亮代理（[PlatformDefaults.proxied]）。
    hosts = listOf("tiktok.com", "tiktokcdn.com", "tiktokcdn-us.com", "ibytedtos.com"),
    capabilities = PlatformCapabilities(
        // 关注流要 ticket-guard，这一阶段不做，所以只有推荐流。
        feeds = setOf(FeedKind.RECOMMEND),
        playableVideo = true,
        // 有真实的图文作品（imagePost），「只看图文」照样请求
    ),
    defaults = PlatformDefaults(feedOn = true, proxied = true, autoTranslate = true),
    login = PlatformLogin(
        web = LoginSpec(
            webLoginUrl = "https://www.tiktok.com/login",
            cookieUrl = "https://www.tiktok.com/",
            cookieDomain = ".tiktok.com",
            // 这里按最常见的那个提示
            requiredCookies = setOf("sessionid"),
        ),
        // 网页登录的 Cookie 在 .tiktok.com（sessionid / sid_tt / multi_sids 是 HttpOnly，只有从 Chrome 的 Cookie 库里导才行）
        chromeImport = CookieImportSpec("tiktok.com"),
    ),
    overseas = true,
    factory = ::RustTiktokSource,
)
