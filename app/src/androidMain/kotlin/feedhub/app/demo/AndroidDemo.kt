package feedhub.app.demo

import feedhub.plugins.InstalledPlugins
import android.content.Context
import android.graphics.Bitmap
import android.graphics.Canvas
import android.graphics.Color
import android.graphics.LinearGradient
import android.graphics.Paint
import android.graphics.RectF
import android.graphics.Shader
import feedhub.app.AndroidSystemActions
import feedhub.app.AppGraph
import feedhub.app.captcha.AndroidCaptchaHandler
import feedhub.app.PROXY_LOG_TAG
import feedhub.app.androidProxyCore
import feedhub.app.imageCacheDir
import feedhub.app.library.Library
import feedhub.app.login.AndroidWebLogin
import feedhub.app.sync.LoginSync
import feedhub.core.Block
import feedhub.core.Comment
import feedhub.core.CommentSort
import feedhub.core.Cursor
import feedhub.core.Danmaku
import feedhub.core.DetailTarget
import feedhub.core.FeedDetail
import feedhub.core.FeedItem
import feedhub.core.FeedKind
import feedhub.core.ItemKind
import feedhub.core.LoginSpec
import feedhub.core.Media
import feedhub.core.MemoryCookieJar
import feedhub.core.MemoryKeyValueStore
import feedhub.core.Page
import feedhub.core.Platform
import feedhub.core.QrLogin
import feedhub.core.QrLoginSource
import feedhub.core.QrLoginState
import feedhub.core.PlayInfo
import feedhub.core.ProgressiveStream
import feedhub.core.Quality
import feedhub.core.RawRequest
import feedhub.core.RawResponse
import feedhub.core.RichText
import feedhub.core.SearchCapability
import feedhub.core.SearchRequest
import feedhub.core.SearchSource
import feedhub.core.Source
import feedhub.core.SourceContext
import feedhub.core.Span
import feedhub.core.Stats
import feedhub.core.UserRef
import feedhub.core.VideoCodec
import feedhub.core.VideoInfo
import feedhub.core.VideoPart
import feedhub.core.VideoSource
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.delay
import okhttp3.Interceptor
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.OkHttpClient
import okhttp3.Protocol
import okhttp3.Response
import okhttp3.ResponseBody.Companion.toResponseBody
import java.io.ByteArrayOutputStream
import java.io.IOException
import java.nio.file.Paths
import kotlin.math.max
import kotlin.math.min

/**
 * 安卓上的演示环境（`androidGraph(demo = true)`，也就是现在 MainActivity 用的那份）：**一个请求也不发**。
 *
 * 和桌面截图工具的 `fake=xhs` 不一样：那边用真的小红书 Source + 录下来的响应 + AWT 现画图片，图片要读仓库里的
 * fixtures 文件；安卓这边跑在 APK 里，没有仓库目录、也没有 AWT，所以这里全部现生成：
 *
 * - [DemoSource]：每个平台各一个，信息流 / 详情 / 评论 / 楼中楼都是代码里拼出来的（真的解析不了，但字段是全的）；
 * - [DemoImages]：图片地址指向 [DEMO_HOST]，由这个拦截器用 `android.graphics` 现画一张同比例的 PNG 返回，
 *   不去 xhscdn、也不看网络状态（Coil 走的是 `AppGraph.imageFetch`，见 [demoImageHttp]）；
 * - 视频：`play` 返回的地址永远不会被用到——安卓的播放器这一阶段是占位的（`UnavailablePlayer`），
 *   `PlaybackSession.loadPart` 看到 `unavailable` 就停在原地，界面显示"这台设备还不能播放"。
 *
 * 第二阶段接真实数据时：把 [androidDemoGraph] 换成 `androidGraph(demo = false)`（真实的 `registry`），
 * 登录 / Cookie 持久化做完之前别在模拟器里打开它。
 */
private const val DEMO_HOST = "demo.feedhub.local"

/** 演示片段几秒（assets 里那两段 mp4 的长度）。 */
private const val DEMO_CLIP_SECONDS = 8

private fun imageUrl(seed: String, w: Int, h: Int) = "https://$DEMO_HOST/img/${w}x$h/$seed.png"

/**
 * 九图演示那一条用的图片地址：**写成 B站 CDN 的样子**（`i0.hdslb.com/bfs/...`），这样"卡片要缩略图 /
 * 九宫格第 2～9 张预取"这套真实规则在演示环境里也会跑（`CardThumbnails` 会把地址改写成 `@480w.webp`）。
 * 图还是 [DemoImages] 现画的 PNG，不出设备。
 */
private const val DEMO_CDN_HOST = "i0.hdslb.com"

private fun cdnImageUrl(seed: String, w: Int, h: Int) = "https://$DEMO_CDN_HOST/bfs/demo/img/${w}x$h/$seed.png"
private fun avatarUrl(seed: String) = "https://$DEMO_HOST/avatar/$seed.png"
private fun emojiUrl(name: String) = "https://$DEMO_HOST/emoji/$name.png"

/**
 * 演示条目里用的两个表情：插件**随带表情图**的平台（描述里有 `emotes`：酷安、知乎）用它表里的前两个——地址是
 * `feedhub-emoji:`，走的就是真机上读插件资源的那条路（`BundledEmoteFetcher`）；别的平台用 [DemoImages] 现画的。
 */
private fun demoEmotes(p: Platform): List<Pair<String, String>> =
    InstalledPlugins.catalog[p]?.emotes?.codes?.entries?.take(2)?.map { it.key to it.value }?.takeIf { it.size == 2 }
        ?: listOf("[doge]" to emojiUrl("doge"), "#(笑眼)" to emojiUrl("xiaoyan"))

/** 演示用的图片客户端：一切图片都由 [DemoImages] 现画（`AppGraph.imageFetch` 底下就是它）。 */
private fun demoImageHttp(): OkHttpClient = OkHttpClient.Builder().addInterceptor(DemoImages()).build()

/**
 * 演示用的"视频片段"拦截器：`https://demo.feedhub.local/media/<名字>.mp4` 从 **debug 的 assets**
 * （`androidApp/src/debug/assets/demo/` 里的 portrait.mp4 / landscape.mp4）读出来原样返回，别的地址照旧放行。
 *
 * 为什么要它：安卓演示环境原来 `play()` 只给档位、没有可播的流，沉浸界面 / 播放器在模拟器上只能停在
 * "没有可播放的流"。画面上烧着
 * 档位名和时间戳），地址还是演示域名，**一个外网请求都没有**。assets 只在 debug 里有，release 里拿不到
 * （getOrNull → 空 body，反正 release 走不到演示数据）。
 */
private class DemoMedia(private val context: Context) : Interceptor {
    override fun intercept(chain: Interceptor.Chain): Response {
        val req = chain.request()
        if (req.url.host != DEMO_HOST || !req.url.encodedPath.startsWith("/media/")) return chain.proceed(req)
        val name = req.url.encodedPath.substringAfterLast('/')
        val bytes = runCatching {
            context.assets.open("demo/$name").use { it.readBytes() }
        }.getOrNull()
        android.util.Log.i("feedhub-demo", "media $name -> ${bytes?.size ?: -1} 字节")
        return Response.Builder().request(req).protocol(Protocol.HTTP_1_1)
            .code(if (bytes != null) 200 else 404).message(if (bytes != null) "OK" else "Not Found")
            .header("Cache-Control", "no-store")
            .body((bytes ?: ByteArray(0)).toResponseBody("video/mp4".toMediaType()))
            .build()
    }
}

/**
 * 演示源的信息流演成什么样。
 * 平时是 [NORMAL]（有内容），空状态截图才换。
 */
enum class DemoFeedMode {
    NORMAL,

    /** 平台回"没登录"（[feedhub.core.NotLoggedInException]）。 */
    NO_LOGIN,

    /** 登录着但这一刷没有内容（空页）。 */
    EMPTY,

    /** 断网（`IOException`）：空状态"没有网络 + 重试"（老那条，验证没被改坏）。 */
    ERROR,
}

/**
 * 安卓演示环境的 [AppGraph]。`library` 传 `filesDir` 下的真库（收藏 / 历史在重启后还在），见 [androidGraph]。
 *
 * [captcha] 是酷安评论的人机验证 handler；演示源不会触发它，但 debug 的「假验证页」入口会直接调它的 `solve`
 * （见 `DebugStartActivity` 的 `--es captcha fake`），覆盖层走的就是真实那条路。
 */
fun androidDemoGraph(
    context: Context,
    library: Library,
    only: Set<Platform>? = null,
    captcha: AndroidCaptchaHandler = AndroidCaptchaHandler(),
    /** 空状态截图：演示源演成"这一刷没内容 / 平台说没登录"。 */
    feedMode: DemoFeedMode = DemoFeedMode.NORMAL,
    /**
     * 首帧截图：演示源取信息流时先等这么多毫秒（`--es netdelay`）。
     * 演示数据是本地拼的，平时一瞬间就回来了，截不到"内容还没上屏"那一帧。
     */
    feedDelayMs: Long = 0,
    /**
     * 「X 式」翻译界面的演示：接上**假引擎**（[feedhub.app.translate.DemoTranslationEngine]，
     * 不起 llama-server、不读 GGUF）并把总开关打开，没有自动翻的平台（手机上默认点了才翻）。
     *
     * 语言识别仍然是真的（Rust 里编进去的 lingua 中 / 英两张模型）：所以"这条是不是中文"和线上一致。
     * 只在 `--ez translate true` 时打开（见 `DebugStartActivity`），正常演示 / 真实数据都不受影响。
     */
    translateDemo: Boolean = false,
): AppGraph {
    // 「X 式」翻译界面的演示：先把演示文字切成英文（中文不会出「翻译」入口），
    // 再构造 Source（DemoSource 读的是这个开关）。必须在建 sources 之前。
    DemoItems.translateDemo = translateDemo
    val jar = MemoryCookieJar()
    // 演示环境的播放片段（`/media/*.mp4`）从这里出；别的地址照旧
    val http = OkHttpClient.Builder().cookieJar(jar).addInterceptor(DemoMedia(context)).build()
    val ctx = SourceContext(http, jar, MemoryKeyValueStore())
    // 和真实数据同一个平台集合，见 feedhub.app.androidPlatforms
    val sources: Map<Platform, (SourceContext) -> Source> =
        feedhub.app.androidPlatforms(only).associateWith { p -> { _: SourceContext ->
            // 描述里 `login.qrLogin = true` 的平台给一个带扫码登录的演示源（弹框能看，二维码是假的）
            val qr = InstalledPlugins.catalog[p]?.login?.qrLogin == true
            (if (qr) DemoQrSource(p, feedMode, feedDelayMs) else DemoSource(p, feedMode, feedDelayMs)) as Source
        } }
    val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main)
    // 演示 / 截图也把核心装上（设置页的「内置核心」档和进阶设置页的核心那几张卡才画得出来），
    // 但**用单独的目录、而且不查订阅自动更新**：绝不会读写用户真实的那份 proxy-core.json，
    // 也不会因为"有订阅到点了"而发任何请求（演示环境一个请求都不发）。
    val core = androidProxyCore(
        context,
        scope,
        dir = Paths.get(context.filesDir.path, "demo-proxy"),
        autoUpdateCheckMs = 0,
    )
    val graph = AppGraph(
        ctx, scope,
        // 演示的"网络"是拦截器现画的（`DemoImages`），所以走 OkHttp 那条路
        imageFetchBase = feedhub.app.image.OkHttpImageFetch(demoImageHttp()),
        imageCacheDir = imageCacheDir(context), library = library,
        systemActions = AndroidSystemActions(context),
        desktop = false,
        sources = sources,
        unavailable = feedhub.app.androidUnavailable(only),
        // 演示源的 LoginSpec 是空的（它本来就不联网，也没有"网页登录"这回事）：
        // 安卓上的"我已 → 账号 / 网页登录页"截图用真实数据 + `--es platforms bili`（见 DebugStartActivity），
        // 演示数据这条路一个请求都不发。
        webLogin = AndroidWebLogin(),
        // 演示环境也能走「登录互传」：它只连同一局域网里的另一台设备（测试就是本机回环），
        // whoami 走 DemoSource（不发任何平台请求），所以这种端到端验证的在线请求数是 0。
        loginSync = LoginSync(canOffer = true, canScan = true, manualQrInput = true),
        proxyCore = core,
        coreForeground = core != null,
        showBuiltinProxy = core != null,
        logLine = { line -> android.util.Log.i(PROXY_LOG_TAG, line) },
        // 演示环境也接上帧率请求（截图 / 手点时能看出"滑动要高刷、静止撤回"），
        // 拿不到 Activity 就空实现（和真实数据那条同一个判断，见 AndroidGraph）
        frameRateSink = (context as? android.app.Activity)?.let { feedhub.app.display.WindowFrameRateSink(it) }
            ?: feedhub.app.display.NoFrameRateSink,
        captcha = captcha,
        captchaPrompt = captcha.prompt,
        // 演示环境也接上本地向量和导出：模拟器上整条通路（算 → 存 → 导出）要能跑，且一个平台请求都不发
        embedder = feedhub.app.androidEmbedder(context, scope, { line -> android.util.Log.i("feedhub-embed", line) }),
        behaviorExport = feedhub.app.export.AndroidBehaviorExport(context),
        // 「X 式」翻译界面：只有 `--ez translate true` 时接假引擎（不启 llama-server）
        translationEngineFactory = if (translateDemo) {
            { _ -> feedhub.app.translate.DemoTranslationEngine() }
        } else {
            null
        },
        // 演示的翻译缓存只放内存：绝不写用户的 translations.jsonl
        translationCacheDir = null,
    )
    // 「X 式」翻译界面：`--ez translate true` 时把总开关打开、平台一个都不点亮
    // （手机上"点了才翻"），档位由 `--es trmode` 决定（ok / fail / loading）。
    if (translateDemo) {
        graph.settings.override { it.copy(translationOn = true, translationPlatforms = emptySet()) }
    }
    // 演示搜索：模拟器上走"搜索 → 点结果 → 详情"那几条场景（双栏下的 a～d）。
    // 演示源本来没有 SearchSource（搜索页会是空的、连平台标签都没有），这里按同一批演示条目造一份，
    // **一个请求都不发**；真机 / 真实数据那条路完全不受影响。
    graph.searchOverride = { sources.keys.map { p -> DemoSearchSource(p) } }
    return graph
}

/**
 * 演示搜索源（只在 `--ez demo true` 下装，见 [androidDemoGraph]）：把演示信息流的同一批条目当搜索结果，
 * 标题前面加上搜索词。**一个请求都不发**——`Source` 那一半委托给一份普通的 [DemoSource]，
 * 所以点开搜索结果以后的详情 / 评论 / 播放和首页那条一模一样（本机演示片段）。
 *
 * 一页就到底（第二次翻页回空）：模拟器上只是走"点开 / 关掉"那几个场景，不需要真的翻页。
 */
internal class DemoSearchSource(
    platform: Platform,
) : VideoSource by DemoSource(platform), SearchSource {
    override val platform: Platform = platform
    override val search: SearchCapability = SearchCapability(pageSize = 6)

    override suspend fun search(request: SearchRequest, cursor: Cursor?): Page<FeedItem> {
        if (cursor != null) return Page(emptyList(), null)
        val hits = DemoItems.feed(platform, FeedKind.RECOMMEND)
            .map { it.copy(title = "搜索「${request.text}」${it.title}") }
        return Page(hits, null)
    }
}

/**
 * 演示源 + 扫码登录（[QrLoginSource]）：让模拟器 / 截图上能看到**真的扫码弹框**（尺寸、文案、轮询节奏都是真的）。
 * 二维码内容是一段**固定的假链接**（扫不出任何东西），轮询永远回"未扫"，**一个请求都不发**。
 *
 * 只在描述里 [feedhub.core.plugin.PlatformLogin.qrLogin] = true 的平台上装（B站 / 知乎 / 贴吧 / 小黑盒 / 小红书 / 抖音），
 * 这样演示环境里"哪些平台有扫码"和真机一致。
 *
 * **按 `VideoSource` 委托**（不是按 `Source`）：`VideoSource` 继承 `NativeDetailSource`，详情 / 评论 / 楼中楼 / 播放
 * 都得透过来。这六家的演示详情在模拟器上会直接崩
 * `"tieba 的演示源没有原生详情"`（`androidDemoItem` 的 `as? NativeDetailSource` 拿到 null），
 */
internal class DemoQrSource(
    platform: Platform,
    feedMode: DemoFeedMode = DemoFeedMode.NORMAL,
    feedDelayMs: Long = 0,
) : QrLoginSource, VideoSource by DemoSource(platform, feedMode, feedDelayMs) {
    override suspend fun startQrLogin(): QrLogin = QrLogin(
        platform = platform,
        content = "https://demo.feedhub.local/qr/${platform.key}",
        token = "demo-${platform.key}",
        pollIntervalMs = 4000,
        hint = "演示环境：这张二维码是假的（用${platform.label} App 扫不出东西）",
    )

    override suspend fun pollQrLogin(login: QrLogin): QrLoginState = QrLoginState.Waiting
}

/**
 * 演示源：所有平台共用一套生成逻辑，只有标题 / 类型按平台换花样。
 *
 * [feedMode] 只给空状态截图用（`DebugStartActivity` 的 `--es emptyfeed`）：演示数据平时总有内容，
 * 想截"空的时候长什么样"就得让信息流这一路空出来。
 */
internal class DemoSource(
    override val platform: Platform,
    private val feedMode: DemoFeedMode = DemoFeedMode.NORMAL,
    /** 首帧截图用（`--es netdelay`）：取信息流前先等这么多毫秒，让"还没上屏"那一帧停留得久一点。 */
    private val feedDelayMs: Long = 0,
) : VideoSource {
    // 和插件描述里真实的支持情况一致（小黑盒 / 抖音 / TikTok 没有关注流），设置页里"关注"那一行的灰才对
    override val feeds = InstalledPlugins.catalog[platform]?.capabilities?.feeds ?: setOf(FeedKind.RECOMMEND, FeedKind.FOLLOWING)
    override val login = LoginSpec("", "", "", emptySet())
    override val apiBase = ""

    override suspend fun whoami(): UserRef = UserRef(platform, "${platform.key}-me", "演示账号（${platform.label}）", avatarUrl("me-${platform.key}"))

    override suspend fun feed(kind: FeedKind, cursor: Cursor?): Page<FeedItem> {
        // 首帧截图（`--es netdelay`）：**第一页**先等一会儿，让"内容还没上屏"那一帧停留得久一点；
        // 加载更多（有游标）不慢，别把翻页也拖长。0 = 平时那样，一瞬就回来。
        if (feedDelayMs > 0 && cursor == null) delay(feedDelayMs)
        return when (feedMode) {
            // 平台回"没登录"：走的是真实那条路（FeedState.notLoggedIn → 空状态"要先登录"）
            DemoFeedMode.NO_LOGIN -> throw feedhub.core.NotLoggedInException(platform, "演示：没登录")
            // 登录着、但这一刷就是没内容
            DemoFeedMode.EMPTY -> Page(emptyList(), null)
            DemoFeedMode.ERROR -> throw IOException("演示：网络错误")
            DemoFeedMode.NORMAL -> if (cursor != null) Page(emptyList(), null) else Page(DemoItems.feed(platform, kind), null)
        }
    }

    override suspend fun detail(id: String): FeedDetail = DemoItems.detail(platform, id)

    override suspend fun comments(id: String, cursor: Cursor?, sort: CommentSort): Page<Comment> =
        if (cursor != null) Page(emptyList(), null) else Page(DemoItems.comments(platform), null)

    override suspend fun replies(id: String, commentId: String, cursor: Cursor?): Page<Comment> =
        Page(DemoItems.replies(platform, commentId), null)

    /**
     * 播放地址：一条**真的能播**的本机演示片段（`androidApp/src/debug/assets/demo/` 里那两段 mp4，由 [DemoMedia] 这个拦截器
     * 从 assets 读出来返回，地址还是 [DEMO_HOST]，所以一个外网请求都没有）。
     * 竖屏条目给竖屏片段、横屏给横屏：沉浸界面 / 横竖屏适配才有东西看。
     */
    override suspend fun play(id: String, partId: String, quality: Int?): PlayInfo {
        val portrait = DemoItems.isPortrait(platform, id)
        android.util.Log.i("feedhub-demo", "play id=$id partId=$partId portrait=$portrait")
        val w = if (portrait) 720 else 1280
        val h = if (portrait) 1280 else 720
        return PlayInfo(
            durationMs = DEMO_CLIP_SECONDS * 1000L,
            quality = 64,
            qualities = listOf(Quality(64, "720P 演示")),
            progressive = listOf(
                ProgressiveStream(
                    64,
                    "https://$DEMO_HOST/media/${if (portrait) "portrait" else "landscape"}.mp4",
                    codec = VideoCodec.AVC,
                    width = w,
                    height = h,
                    bandwidth = 300_000,
                ),
            ),
        )
    }

    override suspend fun danmaku(id: String, partId: String, segment: Int): List<Danmaku> = emptyList()

    override suspend fun raw(request: RawRequest): RawResponse = throw IOException("演示环境：不联网")
}

/** 演示内容：信息流、详情、评论都是这里现拼的。 */
private object DemoItems {
    /**
     * 「X 式」翻译界面的演示：`--ez translate true` 时演示文字换成英文
     * （中文内容不会出「翻译」入口，截不到要截的那几张图）。
     *
     * 只影响**演示数据**（各插件 `src/demo` 那一套；真实数据一个字节都不变）。
     * 由 [androidDemoGraph] 在构造 Source 之前置位。
     */
    @Volatile var translateDemo: Boolean = false

    private const val PER_FEED = 6
    private val now get() = System.currentTimeMillis() / 1000

    fun id(p: Platform, i: Int) = "${p.key}-$i"

    fun feed(p: Platform, kind: FeedKind): List<FeedItem> =
        (0 until PER_FEED).map { item(p, it, following = kind == FeedKind.FOLLOWING) }

    fun detail(p: Platform, id: String): FeedDetail {
        val i = index(id)
        val item = item(p, i, following = false).copy(id = id)
        val video = item.kind == ItemKind.VIDEO
        return FeedDetail(
            item = item,
            body = RichText(body(p, i, video)),
            video = if (video) {
                val v = item.media.filterIsInstance<Media.Video>().firstOrNull()
                VideoInfo(
                    // 宽高照条目自己的来：竖屏那条（i % 4 == 3）在沉浸界面里才认得出是竖屏
                    parts = listOf(
                        VideoPart(
                            "p1", "P1 演示片段",
                            durationSec = DEMO_CLIP_SECONDS,
                            width = v?.width ?: 1280,
                            height = v?.height ?: 720,
                        ),
                    ),
                    // 只有声明了弹幕能力的平台（B站）有弹幕；别的平台留 null，界面据此不显示弹幕开关（和真实情况一致）
                    danmakuSegmentMs = if (hasDanmaku(p)) 360_000 else null,
                )
            } else {
                null
            },
        )
    }

    /**
     * 评论。**带图的几条**：第 1 条一张图、第 2 条三张图（看网格），第 1 条的楼中楼预览里也各带一张 / 两张图
     * 。图都是 [imageUrl]（本机画，不联网）。
     */
    fun comments(p: Platform): List<Comment> = if (translateDemo) {
        // 「X 式」翻译界面演示：第 1 条英文（评论下面出小一号的「翻译」）、第 2 条中文（不出）。
        listOf(
            Comment(
                platform = p,
                id = "${p.key}-c0",
                author = UserRef(p, "${p.key}-cu0", "Dana Reyes", avatarUrl("c-${p.key}-0")),
                content = RichText(listOf(Block.Paragraph(listOf(Span.Text("The deterministic build cache is the one I keep " +
                    "coming back to. It survived a full toolchain upgrade without a single stale hit, which is more " +
                    "than I can say for ours."))))),
                publishedAt = now - 600,
                likes = 42,
                language = "en",
            ),
            Comment(
                platform = p,
                id = "${p.key}-c1",
                author = UserRef(p, "${p.key}-cu1", "演示中文评论者", avatarUrl("c-${p.key}-1")),
                content = RichText(listOf(Block.Paragraph(listOf(Span.Text("这条是中文评论，下面同样不该出现「翻译」。"))))),
                publishedAt = now - 1_200,
                likes = 7,
                language = "zh",
            ),
        )
    } else (0 until 3).map { k ->
        val id = "${p.key}-c$k"
        val pics = when (k) {
            0 -> listOf(photo("${p.key}-c0-p0"))
           1 -> (0 until 3).map { i -> photo("${p.key}-c1-p$i") }
            else -> emptyList()
        }
        Comment(
            platform = p,
            id = id,
            author = UserRef(p, "${p.key}-cu$k", "演示评论者${k + 1}", avatarUrl("c-${p.key}-$k")),
            content = textWithImages("演示评论第 ${k + 1} 条：数据是本地拼的，点开会看到楼中楼也是假的。", pics),
            publishedAt = now - 600L * (k + 1),
            likes = (k + 1) * 12L,
            replyCount = if (k == 0) 2L else null,
            ipLocation = "演示",
            previewReplies = if (k == 0) replies(p, id) else emptyList(),
        )
    }

    fun replies(p: Platform, commentId: String): List<Comment> = (0 until 2).map { k ->
        Comment(
            platform = p,
            id = "$commentId-r$k",
            author = UserRef(p, "${p.key}-ru$k", "演示回复者${k + 1}", avatarUrl("r-${p.key}-$k")),
            content = textWithImages(
                "演示楼中楼第 ${k + 1} 条。",
                if (k == 0) listOf(photo("${p.key}-r0-p0")) else (0 until 2).map { i -> photo("${p.key}-r1-p$i") },
            ),
            publishedAt = now - 300L * (k + 1),
            likes = (k + 1) * 3L,
            replyTo = UserRef(p, "$commentId-u", "演示评论者1"),
            ipLocation = "演示",
        )
    }

    private fun photo(seed: String): Block.Image = Block.Image(imageUrl(seed, 1200, 900), width = 1200, height = 900)

    private fun textWithImages(text: String, images: List<Block.Image>): RichText =
        RichText(listOf<Block>(Block.Paragraph(listOf(Span.Text(text)))) + images)

    /** 第 i 条是不是"九图帖子"（演示用：i == 2 的图文帖带 9 张图，详情里是三列九宫格）。 */
    private fun nineImages(i: Int) = i == 2

    /**
     * 九图帖子的 9 个图片地址：**卡片和正文用同一批**（详情页按"图片身份"认得出卡片上屏时预取的缩略图），
     * 地址写成 B站 CDN 的样子，好让 [feedhub.app.feed.CardThumbnails] 真的去改写缩略图。
     */
    private fun nineImages(p: Platform, i: Int): List<String> =
        (0 until 9).map { k -> cdnImageUrl("${p.key}-grid$i-$k", 1200, 900) }

    private fun index(id: String): Int = id.substringAfterLast('-').toIntOrNull()?.coerceIn(0, PER_FEED - 1) ?: 0

    /** 这条演示视频是不是竖屏（和 [item] 里 `i % 4 == 3` 那条规则同一处，播放片段按它挑竖 / 横）。 */
    fun isPortrait(p: Platform, id: String): Boolean {
        val i = index(id)
        val video = when (p.key) {
            "bili", "heybox" -> i % 2 == 0
            "douyin", "yt" -> true
            else -> i % 3 == 0
        }
        return video && i % 4 == 3
    }

    private fun item(p: Platform, i: Int, following: Boolean): FeedItem {
        // 演示数据的花样按平台 id 换（只是演示内容，不是能力判断；没列的平台用通用的）
        val video = when (p.key) {
            "bili", "heybox" -> i % 2 == 0
            "douyin", "yt" -> true
            else -> i % 3 == 0
        }
        // 每四条里有一条竖屏（看卡片按视频自己的比例画）
        val portrait = video && i % 4 == 3
        val w = if (portrait) 720 else 1280
        val h = if (portrait) 1280 else 720
        val id = id(p, i)
        val media = if (video) {
            listOf(Media.Video(cover = imageUrl("${p.key}-v$i", w, h), durationSec = 62 + i * 37, width = w, height = h))
        } else if (nineImages(i)) {
            // 九图帖子（i == 2）：卡片上带 9 张图 → 详情里是三列九宫格，第 2～9 张靠卡片上屏时预取的缩略图
            nineImages(p, i).map { Media.Image(it, 1200, 900) }
        } else {
            List(1 + i % 3) { k -> Media.Image(imageUrl("${p.key}-p$i-$k", 1200, 900), 1200, 900) }
        }
        val kind = when {
            video -> ItemKind.VIDEO
            p.key == "zhihu" -> if (i % 2 == 0) ItemKind.ANSWER else ItemKind.ARTICLE
            p.key == "bili" || p.key == "xhs" -> ItemKind.MOMENT
            else -> ItemKind.POST
        }
        return FeedItem(
            platform = p,
            id = id,
            kind = kind,
            title = title(p, i),
            summary = summary(p, i),
            author = UserRef(p, "${p.key}-u$i", "演示作者${i + 1}", avatarUrl("u-${p.key}-$i")),
            // 发布时间错开：各平台之间差一档，首页整批按时间排的时候能看出交错
            publishedAt = now - i * 3600L - InstalledPlugins.catalog.platforms.indexOf(p).coerceAtLeast(0) * 900L,
            media = media,
            stats = Stats(
                views = 1200L + i * 371L,
                likes = 60L + i * 17L,
                comments = 3L + i,
                danmaku = if (video && hasDanmaku(p)) 88L + i * 13L else null,
                favorites = if (!video) 5L + i else null,
            ),
            reason = if (following) "已关注" else null,
            community = if (InstalledPlugins.catalog[p]?.texts?.communitySuffix == "吧") "演示吧" else null,
            emotes = demoEmotes(p).toMap(),
            url = "https://$DEMO_HOST/post/${p.key}/$i",
            // 「X 式」翻译界面演示：平台说这条是英文（真 App 里 X / YouTube 会给 lang 元数据），
            // 语言识别那条路也照样跑（不给也能判出来，这里给上是为了更贴近线上）
            language = if (translateDemo) "en" else null,
            detail = DetailTarget.Native(id),
        )
    }

    private fun title(p: Platform, i: Int): String = if (translateDemo) {
        // 「X 式」翻译界面演示：英文标题（界面判定为外语 → 正文下面出「翻译」入口）
        "Demo post ${i + 1}: six projects from the last few weeks that caught our attention"
    } else when (p.key) {
        "bili" -> "【演示】${i + 1} 分钟看懂 Compose Multiplatform 的一套界面出两个包"
        "zhihu" -> "演示回答：为什么聚合信息流要按发布时间整批排？"
        "tieba" -> "【演示帖】吧友的安卓平板横屏体验（第 ${i + 1} 版）"
        "heybox" -> "演示：小黑盒的图文帖长这样"
        "coolapk" -> "演示动态：酷安的表情和九宫格缩略图"
        "xhs" -> "演示笔记 ${i + 1}：手机竖屏的瀑布流卡片"
        "douyin" -> "演示视频 ${i + 1}：本机 assets 里的测试片段（时间在走就是真的在播）"
        "x" -> "演示推文 ${i + 1}：X 的卡片长这样"
        "yt" -> "演示视频 ${i + 1}：YouTube 的 TV 首页卡片"
        "reddit" -> "演示帖子 ${i + 1}：Reddit 的图文帖和评论树"
        "tiktok" -> "演示视频 ${i + 1}：TikTok 的竖屏卡片"
        else -> "演示内容 ${i + 1}：${p.label}的卡片长这样"
    }

    private fun hasDanmaku(p: Platform) = InstalledPlugins.catalog[p]?.capabilities?.danmaku == true

    private fun summary(p: Platform, i: Int): String = if (translateDemo) {
        "Demo summary ${i + 1}: a quiet but very productive stretch, with six open-source projects worth a look. " +
            "Everything here is generated locally, so the emulator never sends a single request."
    } else {
        "演示摘要 ${i + 1}：这一段文字里混了平台表情 ${demoEmotes(p)[0].first} 和 ${demoEmotes(p)[1].first}，卡片上应该画成小图而不是文字代码。" +
            "数据全部来自本地生成，模拟器里不会发出任何请求。"
    }

    private fun body(p: Platform, i: Int, video: Boolean): List<Block> = if (translateDemo) {
        // 「X 式」翻译界面演示：英文正文（判定为外语 → 正文下面那一行出「翻译」）
        buildList {
            add(Block.Paragraph(listOf(Span.Text("Demo body ${i + 1}: six projects from the last few weeks that caught our attention. " +
                "None of them are famous yet, and that is exactly why they are worth reading: small teams, sharp scope."))))
            add(Block.Heading(listOf(Span.Text("Why these six"))))
            add(Block.Paragraph(listOf(Span.Text("Each one solves a problem its authors hit themselves: a local-first notes index, " +
                "a deterministic build cache, and a terminal image viewer that finally gets colour right."))))
            add(Block.Quote(listOf(Span.Text("The best tooling is the kind you only notice when it stops working."))))
            add(Block.Image(url = imageUrl("${p.key}-body$i-0", 1200, 900), width = 1200, height = 900, caption = "Demo image"))
            add(Block.Paragraph(listOf(Span.Text("Links and short write-ups are in the thread below. Everything here is generated " +
                "locally, so the emulator never sends a single request."))))
        }
    } else buildList {
        add(Block.Paragraph(listOf(Span.Text("这是 ${p.label} 的演示正文第 ${i + 1} 篇，整篇都是本地拼的，没有任何网络请求。"))))
        add(Block.Heading(listOf(Span.Text("一、演示小标题"))))
        add(
            Block.Paragraph(
                listOf(
                    Span.Text("正文里的行内表情 "),
                    demoEmotes(p).first().let { (code, url) -> Span.Emote(code, url) },
                    Span.Text(" 与链接 "),
                    Span.Link("演示链接", "https://$DEMO_HOST/"),
                    Span.Text(" 都按各平台一样的画法渲染。"),
                ),
            ),
        )
        if (!video && nineImages(i)) {
            // 九宫格：和卡片上是同一批地址（详情页第 2～9 张先用卡片上屏时预取的缩略图垫着）
            nineImages(p, i).forEachIndexed { k, url ->
                add(Block.Image(url = url, width = 1200, height = 900, caption = "九宫格第 ${k + 1} 张"))
            }
        } else {
            add(Block.Image(url = imageUrl("${p.key}-body$i-0", 1200, 900), width = 1200, height = 900, caption = "正文配图 1"))
        }
        add(Block.Quote(listOf(Span.Text("引用块：这一段按引用样式画。"))))
        add(Block.Code("fun main() {\n    println(\"feedhub android\")\n}", "kotlin"))
        if (!video) {
            // 正文里夹着的视频（点开才加载；安卓这一阶段是"这台设备还不能播放"的占位）
            add(
                Block.Video(
                    id = id(p, i), partId = "seg:1", cover = imageUrl("${p.key}-inline$i", 1280, 720),
                    durationSec = 96, width = 1280, height = 720, title = "正文里夹着的演示视频",
                ),
            )
        }
        add(Block.Divider)
        if (video || !nineImages(i)) {
            add(Block.Image(url = imageUrl("${p.key}-body$i-1", 1200, 1600), width = 1200, height = 1600, caption = "正文配图 2（竖图）"))
        }
        add(Block.Paragraph(listOf(Span.Text("最后一段：详情页的评论、楼中楼、大图查看和顶栏都走同一套代码。"))))
    }
}

/**
 * 演示图片：把 `demo.feedhub.local` 的图片请求现画成 PNG。宽高从地址里的 `<w>x<h>` 读，
 * 颜色由地址的 hash 决定（同一张图每次都一样，方便对比两次截图）。
 */
private class DemoImages : Interceptor {
    override fun intercept(chain: Interceptor.Chain): Response {
        val req = chain.request()
        if (req.url.host != DEMO_HOST && req.url.host != DEMO_CDN_HOST) throw IOException("演示环境：不联网（${req.url.host}）")
        val path = req.url.encodedPath
        val bytes = when {
            path.startsWith("/avatar/") -> avatar(req.url.toString())
            path.startsWith("/emoji/") -> emoji(req.url.toString())
            else -> photo(req.url.toString())
        }
        // 演示环境记一行（看"九宫格第 2～9 张是卡片上屏时取的、不是点进详情才取的"：卡片那会儿就该有 @480w.webp 这行）
        android.util.Log.i("feedhub-demo", "img ${req.url.encodedPath}${req.url.encodedQuery?.let { "?$it" } ?: ""} ${bytes.size}B")
        // no-store：占位图不该进 Coil 的磁盘缓存（改了画法还读到旧的）
        return Response.Builder().request(req).protocol(Protocol.HTTP_1_1).code(200).message("OK")
            .header("Cache-Control", "no-store")
            .body(bytes.toResponseBody("image/png".toMediaType())).build()
    }

    private fun photo(url: String): ByteArray {
        val m = Regex("/(\\d+)x(\\d+)/").find(url)
        val w = m?.groupValues?.get(1)?.toIntOrNull() ?: 1200
        val h = m?.groupValues?.get(2)?.toIntOrNull() ?: 900
        val scale = min(1f, 480f / max(w, 1))
        val iw = max(8, (w * scale).toInt())
        val ih = max(8, (h * scale).toInt())
        val hue = hue(url)
        return png(iw, ih) { canvas, paint ->
            paint.shader = LinearGradient(
                0f, 0f, iw.toFloat(), ih.toFloat(),
                Color.HSVToColor(floatArrayOf(hue, 0.35f, 0.95f)),
                Color.HSVToColor(floatArrayOf((hue + 43f) % 360f, 0.5f, 0.75f)),
                Shader.TileMode.CLAMP,
            )
            canvas.drawRect(0f, 0f, iw.toFloat(), ih.toFloat(), paint)
            paint.shader = null
            paint.color = Color.WHITE
            paint.alpha = 190
            paint.textSize = max(12f, iw / 12f)
            canvas.drawText("${w}x$h", iw / 12f, ih / 2f, paint)
        }
    }

    private fun avatar(url: String): ByteArray = png(96, 96) { canvas, paint ->
        paint.color = Color.HSVToColor(floatArrayOf(hue(url), 0.45f, 0.85f))
        canvas.drawRect(0f, 0f, 96f, 96f, paint)
    }

    /** 小黄脸：卡片摘要 / 正文里的行内表情占位，认得出是张图就行。 */
    private fun emoji(url: String): ByteArray = png(96, 96) { canvas, paint ->
        paint.color = Color.HSVToColor(floatArrayOf(43f + hue(url) % 20f, 0.75f, 1f))
        canvas.drawOval(RectF(4f, 4f, 92f, 92f), paint)
        paint.color = Color.rgb(60, 40, 20)
        canvas.drawOval(RectF(28f, 34f, 38f, 48f), paint)
        canvas.drawOval(RectF(58f, 34f, 68f, 48f), paint)
        paint.style = Paint.Style.STROKE
        paint.strokeWidth = 5f
        canvas.drawArc(RectF(28f, 40f, 68f, 74f), 20f, 140f, false, paint)
        paint.style = Paint.Style.FILL
    }

    private fun hue(url: String) = ((url.hashCode() and 0x7fffffff) % 360).toFloat()

    private inline fun png(w: Int, h: Int, draw: (Canvas, Paint) -> Unit): ByteArray {
        val bitmap = Bitmap.createBitmap(w, h, Bitmap.Config.ARGB_8888)
        val canvas = Canvas(bitmap)
        val paint = Paint().apply { isAntiAlias = true; color = Color.WHITE }
        draw(canvas, paint)
        val out = ByteArrayOutputStream()
        bitmap.compress(Bitmap.CompressFormat.PNG, 100, out)
        bitmap.recycle()
        return out.toByteArray()
    }
}
