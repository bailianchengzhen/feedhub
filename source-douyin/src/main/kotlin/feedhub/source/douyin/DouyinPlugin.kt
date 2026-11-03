package feedhub.source.douyin

import feedhub.core.FeedKind
import feedhub.core.LoginSpec
import feedhub.core.Platform
import feedhub.core.plugin.CookieImportSpec
import feedhub.core.plugin.PlatformCapabilities
import feedhub.core.plugin.PlatformDefaults
import feedhub.core.plugin.PlatformLogin
import feedhub.core.plugin.PlatformPlugin

/** 抖音的平台标识。id `douyin` 定了就别改（存储键、cli 参数、条目 key 都用它）。 */
internal val DOUYIN: Platform = Platform.define("douyin", "抖音")

/** 抖音插件的描述。构建期的那一半在模块根的 `plugin.toml`。 */
val DouyinPlugin: PlatformPlugin = PlatformPlugin(
    platform = DOUYIN,
    color = 0xFFFE2C55,
    // 接口域名 + 图片 / 视频 CDN，给共用的客户端按域名选代理用（见 [feedhub.core.platformForHost]）。
    // `douyinpic.com` 是封面 / 头像的 CDN（真实录制里封面 / 头像都是它）。
    // 抖音**不走代理**（国内平台，设置里默认不点亮）。
    hosts = listOf("douyin.com", "douyinpic.com", "douyinvod.com"),
    capabilities = PlatformCapabilities(
        // 所以只有推荐流。
        feeds = setOf(FeedKind.RECOMMEND),
        // 楼中楼 `comment/list/reply` 在线被 bdturing 人机验证拒了，关着
        nestedReplies = false,
        playableVideo = true,
        // 基本全是视频（图文作品很少）：「只看图文」时不请求
        videoOnlyFeeds = setOf(FeedKind.RECOMMEND, FeedKind.FOLLOWING),
    ),
    defaults = PlatformDefaults(feedOn = true),
    login = PlatformLogin(
        // 扫码登录（QrLoginSource）：账号页两端都显示「扫码登录」
        qrLogin = true,
        web = LoginSpec(
            webLoginUrl = "https://www.douyin.com/",
            cookieUrl = "https://www.douyin.com/",
            cookieDomain = ".douyin.com",
            requiredCookies = setOf("sessionid"),
        ),
        // 网页登录的 Cookie 在 .douyin.com（含 UIFID / sessionid / ttwid），
        chromeImport = CookieImportSpec("douyin.com"),
    ),
    // 没有开关：直接建 Rust 版（包装 DouyinSource，解析 / a_bogus / secsdk 走 Rust）
    factory = { ctx -> RustDouyinSource(ctx) },
)
