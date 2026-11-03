package feedhub.app

import androidx.compose.runtime.snapshotFlow
import feedhub.app.feed.FeedOrder
import feedhub.app.feed.FeedSnapshot
import feedhub.app.feed.FeedState
import feedhub.app.library.Library
import feedhub.app.video.RelayFactory
import feedhub.app.video.createRelay
import feedhub.app.library.EXPOSURE_RETENTION_DAYS
import feedhub.app.library.SEEN_ITEM_RETENTION_DAYS
import feedhub.app.library.seenItemCutoff
import feedhub.app.login.CookieImporter
import feedhub.app.login.WebLogin
import feedhub.app.login.WebLoginProxyPlan
import feedhub.app.login.WebLoginProxyRules
import feedhub.app.library.MemoryLibrary
import feedhub.app.mine.MineState
import feedhub.app.sync.LoginSync
import feedhub.app.captcha.CaptchaPromptState
import feedhub.app.display.FrameRateStateMachine
import feedhub.app.embed.Embedder
import feedhub.app.embed.EmbeddingScheduler
import feedhub.app.display.FrameRateSink
import feedhub.app.display.NoFrameRateSink
import feedhub.app.translate.LanguageIdentifier
import feedhub.app.translate.TranslationEngine
import feedhub.app.translate.TranslationStore
import feedhub.app.video.ActivePlaybackSessions
import feedhub.core.CaptchaHandler
import feedhub.brain.ContentFilter
import feedhub.proxy.ProxyRules
import feedhub.core.DeviceCodeLoginSource
import feedhub.core.FeedItem
import feedhub.core.FeedKind
import feedhub.core.Platform
import feedhub.core.plugin.PlatformCatalog
import feedhub.plugins.InstalledPlugins
import feedhub.core.ProxyConfig
import feedhub.core.ProxyMode
import feedhub.core.ProxyProvider
import feedhub.core.Source
import feedhub.core.SourceContext
import feedhub.core.UserAgents
import feedhub.core.platformHostTable
import feedhub.core.platformForHost
import feedhub.core.proxy.CoreStatus
import feedhub.app.image.DefaultHeadersImageFetch
import feedhub.app.image.ImageFetch
import feedhub.proxy.ProxyCore
import feedhub.rust.ProxyTarget
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.FlowPreview
import kotlinx.coroutines.flow.debounce
import kotlinx.coroutines.flow.drop
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

/**
 * App 里各页面共用的对象：平台 Source、设置、信息流状态。桌面 / 安卓各自组装好 [SourceContext] 传进来。
 *
 * @param scope 主线程的协程作用域（App 活多久它活多久）。
 * @param imageFetchBase 取图的底层实现（产品路径是 `RustImageFetch`，走 `feedhub-net`）：这里会再补一个浏览器 UA
 *   （酷安的图片 CDN 对没有浏览器 UA 的请求回 567）。代理按**域名**逐请求选（见 [imageProxy]）：
 *   各平台的图混在一个信息流里，图片加载器只有一个（见 [imageFetch]）。
 * @param imageCacheDir 图片磁盘缓存目录，null 就只用内存缓存。
 * @param library 本地收藏 / 历史 / 屏蔽。
 * @param systemActions 系统级动作（系统分享面板 / 浏览器 / 官方 App 深链）：安卓给 `AndroidSystemActions`，
 *   桌面用默认的 [SystemActions.None]（= 分享仍复制、外链仍复制，桌面行为不变）。
 * @param cookieImporter "从浏览器导入 Cookie"（桌面版有，安卓没有）。
 * @param desktop 是不是桌面版：桌面版首页卡片也按"默认显示原图"用原图（不缺流量，大屏上缩略图发糊），设置里有"界面缩放"。
 * @param systemUiScale 系统的缩放倍数（桌面版读到的），设置页"自动"后面显示它。
 * @param platforms 装了哪些平台插件（顺序 = 界面顺序），默认是这次构建生成的注册表（`plugins.toml`）。
 *   界面按它画平台标签、按它的能力分支（见 [LocalPlatforms]）；测试 / 演示可以换一份。
 * @param sources 平台 → Source 的工厂，默认取自 [platforms]；演示 / 截图用它换成不联网的假 Source。
 * @param unavailable 这台机器 / 这个构建上暂时用不了的平台，值是给用户看的一行原因。**不可用的平台不进 [sources]，也不构造它的 Source**；界面上灰显。
 * @param webLogin 「网页登录」（系统 WebView）：安卓有（`AndroidWebLogin`），桌面为 null（走扫码 / 从 Chrome 导入）。
 * @param showBuiltinProxy 设置页要不要显示「代理」那四档里的「内置核心」：**这个组装里真的接了核心才显示**
 *   （桌面接了 sing-box；安卓要 `nativeLibraryDir/libsingbox.so` 在，见 `androidProxyCore`）。
 *   没接核心时这一档藏起来，选不到就不会 fail-closed 到莫名其妙的错误上。
 * @param logLine 一行日志写到哪（默认 stderr；安卓组装接到 logcat）。目前只有「出口锁关闭，直连」用它，
 *   见下面的 [proxyProvider]。
 */
class AppGraph(
    baseContext: SourceContext,
    val scope: CoroutineScope,
    /**
     * 取图的**底层**实现。默认 `null` = 产品路径（[RustImageFetch]：走 `feedhub-net`）；
     * 演示 / 截图 / 测试传 [feedhub.app.image.OkHttpImageFetch]，它们的"网络"本来就是拦截器现画的。
     * 浏览器 UA 和按域名的代理由 [imageFetch] 补，所以这里**不需要**是一个配好的客户端。
     */
    imageFetchBase: ImageFetch? = null,
    val imageCacheDir: java.nio.file.Path? = null,
    val library: Library = MemoryLibrary(),
    /** 系统分享面板 / 浏览器 / 官方 App 深链；桌面是 [SystemActions.None]。 */
    val systemActions: SystemActions = SystemActions.None,
    val cookieImporter: CookieImporter? = null,
    val desktop: Boolean = false,
    val systemUiScale: Float? = null,
    val platforms: PlatformCatalog = InstalledPlugins.catalog,
    private val sources: Map<Platform, (SourceContext) -> Source> = platforms.factories,
    /**
     * 各平台会去连的域名（插件描述里的 `hosts`）：图片按域名选代理用。
     * **不用建 Source 就能读**；演示 / 测试换成自己的假 Source 时也可以换一份。
     */
    private val hosts: Map<Platform, List<String>> = platforms.hosts,
    /** 代理设置改动后等多久再换客户端（毫秒）。设置页是边打字边存的，等用户停下来再动。测试里给 0。 */
    private val proxyChangeDebounceMs: Long = 600L,
    /**
     * 内置代理核心。桌面版由
     * `desktopGraph` 组装好传进来；**安卓版由 `androidProxyCore` 组装**（二进制取 `nativeLibraryDir`，
     * 只在前台跑，见 [coreForeground]）；测试 / 截图不给（选了「内置核心」就 fail-closed）。
     * 「进阶设置 → 代理（进阶）」那一页（内置代理核心 / 订阅 / 节点）都在它上面。
     */
    val proxyCore: ProxyCore? = null,
    /**
     * 安卓：**内置核心只在前台跑**的开关：
     * 前台 + 选了「内置核心」才起，退到后台 30 秒后停。安卓组装传 `true`、
     * 由 [MainActivity] 的 `onStart` / `onStop` 调 [applyForeground]；桌面**不给**（桌面没有"退到后台"
     * 这回事，[watchProxyCore] 直接起停）。
     *
     * 这张判定表本身在 Rust（`proxy_core::foreground::ForegroundGate`，30 秒宽限期也一样）。
     */
    val coreForeground: Boolean = false,
    /** 暂时用不了的平台 → 一行原因（见构造参数说明）。 */
    val unavailable: Map<Platform, String> = emptyMap(),
    /** 「网页登录」（系统 WebView）；桌面为 null。 */
    val webLogin: WebLogin? = null,
    /**
     * 「登录互传」：任意两台客户端扫码互传（见 [LoginSync]）。电脑只能出码、手机 / 平板两种都能当。
     * 演示 / 截图 / 测试的组装不给（null），账号页就不显示这个入口。
     */
    val loginSync: LoginSync? = null,
    /**
     * 跨设备同步的选文件层。桌面给 [feedhub.app.sync.DesktopSyncFiles]、
     * 安卓给 [feedhub.app.sync.AndroidSyncFiles]；演示 / 截图 / 测试不给 → 设置页那张卡片只说明、禁用按钮。
     */
    val syncFiles: feedhub.app.sync.SyncFiles? = null,
    /**
     * 跨设备同步的设备密钥。
     * 桌面传 `~/.config/feedhub`、安卓传 `context.filesDir`（`DeviceKeyStore` 只碰自己那个 600 权限文件）；
     * 演示 / 截图 / 测试用临时目录或 null（null = 只支持密码模式）。
     */
    val deviceKeys: feedhub.core.sync.DeviceKeyStore? = null,
    /** 配对时给对面看的设备名（桌面主机名、安卓机型）；空串就显示"这台设备"。 */
    val deviceName: String = "",
    /**
     * 「配对新设备」这个组装支持哪些操作（显示二维码 / 扫码 / 手动粘贴）。
     * 演示 / 测试不给 → 设置页不显示配对入口。
     */
    val pairing: feedhub.app.sync.SyncPairingUi? = null,
    /** 设置页要不要显示「内置核心」那一档。 */
    val showBuiltinProxy: Boolean = true,
    /**
     * 平台读接口要人机验证时的处理人（目前只有酷安评论）。桌面端给
     * [feedhub.app.captcha.DesktopCaptchaHandler]，安卓给 `feedhub.app.captcha.AndroidCaptchaHandler`
     * （见 `androidGraph`）；测试 / 截图不给，Source 会抛一条说得清的错误。
     */
    val captcha: CaptchaHandler? = null,
    /** 验证码弹框的状态（桌面端 handler 写、[App] 读）；没有 handler 就是 null。 */
    val captchaPrompt: CaptchaPromptState? = null,
    /**
     * 本地翻译引擎的工厂。桌面传 [feedhub.app.translate.LlamaTranslationEngine]（llama.cpp + HY-MT），
     * **安卓传 [feedhub.app.translate.AndroidTranslationEngine]**（同一份 Rust，CPU 后端 + assets 里的模型），
     * 测试 / 截图不传 → [translation] 的 `available` 为 false，界面不翻。
     * 用工厂是因为它要用已经建好的 [settings]（见下面的 [translation]）。
     */
    private val translationEngineFactory: ((SettingsStore) -> TranslationEngine?)? = null,
    /** 翻译缓存的落盘目录（桌面是 `~/.cache/feedhub`，安卓是 App 私有缓存目录）；null 就只放内存。 */
    translationCacheDir: java.nio.file.Path? = null,
    /**
     * 本地语言识别：**生产不传**；不给就退化成"没平台语言就翻"。
     * 传了只有一种用途：测试 / 截图注入一个假识别器（会真的过 FFI 回调到 Kotlin）。
     */
    translationIdentifier: LanguageIdentifier? = null,
    /** 一行日志写到哪（默认 stderr）。「出口锁关闭，直连」那行走它，见 [proxyProvider]。 */
    private val logLine: (String) -> Unit = { System.err.println(it) },
    /** 首页 / 关注"上一批"存在哪（桌面版是 `~/.cache/feedhub`，冷启动先显示，见 [FeedSnapshot]）；null 就不存。 */
    private val feedSnapshotDir: java.nio.file.Path? = null,
    /**
     * 刷新率请求落到哪。安卓由 `androidGraph` 传 `WindowFrameRateSink(activity)`
     * （`preferredDisplayModeId` 挑当前分辨率下刷新率最高的显示模式）；**桌面不传 = [NoFrameRateSink]**，
     * 一个系统调用都不发，行为不变。
     */
    frameRateSink: FrameRateSink = NoFrameRateSink,
    /**
     * 本地向量模型：**安卓**给 `OnnxTextEmbedder`（打包时带了模型才 `available`）；
     * **桌面**给 `DesktopOnnxTextEmbedder`；
     * 演示 / 测试 / 截图不给（= [feedhub.app.embed.UnavailableEmbedder]，整条通路安静地关掉）。
     */
    val embedder: Embedder? = null,
    /**
     * 行为数据导出的落盘层：安卓写 `getExternalFilesDir(null)/export/`，桌面写 `~/.config/feedhub/export/`；
     * 不给（演示 / 测试 / 截图）= 设置页那个「导出」按钮禁用。
     */
    val behaviorExport: feedhub.app.export.BehaviorExportTarget? = null,
    /**
     * 性能记录器：安卓组装给一份，桌面 / 演示 / 测试是 null（= 不记录）。
     * 开关在设置里（[AppSettings.perfLog]，默认关）；这里只负责把它接到前后台和设置变化上，见 init。
     */
    val perf: feedhub.app.perf.PerfRecorder? = null,
    /**
     * **桌面 App 的对外接口**：在本机 unix socket 上
     * 给 agent 提供只读 HTTP。
     *
     * **它不是独立程序，也没有自己的状态**：只有桌面组装传一份（[feedhub.app.api.DesktopAppApi]），
     * 请求全部走这个 graph 里的 Source / Cookie / 设置 / 资料库；安卓 / 演示 / 测试不传 = 没有这个接口
     * （设置页也就不显示那一页）。开关在设置里（[AppSettings.appApi]，**默认关**），见 init 里的接线和
     * [shutdown] 里的删除 socket。
     */
    val appApi: feedhub.app.api.AppApi? = null,
    /**
     * **封面向量**：视觉图 + 文本图。
     *
     * 只在安卓的 NPU 包里接（[feedhub.app.embed.NpuOnnxCoverEmbedder]）；桌面 / 演示 / 测试 / 普通包不传 =
     * 这条旁路整个不存在（设置页那一条显示"没带图"）。**它不影响纯文字向量**：两条并存，库里按
     * `item_embedding.model` 分开（[feedhub.app.library.npuCoverModelId]）。
     */
    val coverEmbedder: feedhub.app.embed.CoverEmbedder? = null,
    /**
     * 封面图从哪来（安卓 = Coil 磁盘缓存 + App 的图片客户端，只在**不按流量计费**的网络上取）；
     * 桌面 / 测试不传 = 取不到图（封面那条就只是排队、不出网）。
     */
    val coverImages: feedhub.app.embed.CoverImageSource? = null,
) {
    /**
     * 设置。
     * 不自动翻"）：手机走的是详情里那一行「翻译」，点了才翻；桌面上"点亮平台的自动翻"规则不变。
     * 存档里已经有这个字段的用户（自己在设置里改过）照读他的选择，这里只管"从没存过"的那一份默认值。
     */
    val settings = SettingsStore(
        baseContext.store,
        scope,
        platforms,
        translationDefault = if (desktop) null else emptySet(),
    )

    /**
     * App 现在在前台吗（安卓由 Activity 的 `onStart` / `onStop` 经 [onForeground] 喂；桌面永远 true）。
     * 后台不发平台请求、不预取、不要高刷——见 [AppForeground]。
     */
    val foreground = AppForeground()

    /**
     * 刷新率：**前台 = 屏幕最高，不按场景调**。
     * 由 [onForeground] 喂前后台；桌面是 [NoFrameRateSink]，一个系统调用都不发。
     */
    val frameRate = FrameRateStateMachine(frameRateSink)

    /**
     * 本地向量：后台给新卡片算向量，**当前不参与排序**（权重 0）。
     * 触发点：首页 / 关注的新一批（[FeedState] 的 `onNewContent`）和点开的详情（[onItemOpened]）。
     * 受「记录浏览行为」开关和前后台管，见 [EmbeddingScheduler] 的说明。
     */
    val embedding = EmbeddingScheduler(
        scope = scope,
        library = library,
        embedder = embedder,
        enabled = { settings.value.recordBehavior },
        foreground = { foreground.isForeground },
        log = logLine,
        // 性能记录：每批的后端 / 条数 /
        // 耗时 / 模型 / 长度进 JSON Lines，
        onBatch = { done ->
            perf?.eventEmbedBatch(done.backend.name.lowercase(), done.count, done.dMs, done.model, done.len)
        },
        // 按 token 数分组取批：包里有几张固定长度的图，调度器才知道"这批该多长"。
        // 只有一张（或 CPU）时是空 / 一个，调度器就不去数 token（省掉一次没用的分词）。
        graphLengths = { embeddingBackend?.npuLengths.orEmpty() },
    )

    /**
     * 本地向量的**后端开关面**：只有安卓的 [feedhub.app.embed.RoutingEmbedder]
     * 实现了它；桌面 / 测试 / 没接路由的组装是 null（设置页就只有一个灰显的开关 + "这个包没带 NPU 模型"）。
     *
     * 开关变了要**立刻**生效（不用重启 App），所以 init 里用 `snapshotFlow` 盯着 [AppSettings.embedNpu]。
     */
    val embeddingBackend: feedhub.app.embed.EmbeddingBackendControl? =
        embedder as? feedhub.app.embed.EmbeddingBackendControl

    /**
     * **封面向量**：一批一个"封面窗口"地算，算完存库
     * （`item_embedding.model = …-img8x8`）。**权重仍然是 0**，首页顺序一条都不变。
     *
     * 和纯文字那条的关系：**并存**。不吃纯文字的队列，纯文字闲着才动手；只有第一次编图时
     * 才让纯文字把 QNN 会话放掉（见 `TextSessionGate` / `NpuOnnxCoverEmbedder`）。
     */
    val coverVectors = feedhub.app.embed.CoverVectorScheduler(
        scope = scope,
        library = library,
        embedder = coverEmbedder,
        images = coverImages,
        // 三个开关都要开：「记录浏览行为」管整条向量通路，「封面向量（实验）」只管这一条旁路，
        // 「用 NPU 算」必须有——封面向量只有 NPU 这条路（两张 HTP 图），而且它和纯文字**共用**那份
        // 271 MB 权重：那份权重是纯文字那条（NPU 路径）解包出来的，NPU 关着的时候它根本不在私有目录里。
        // 不开这个门的话每个进程第一次窗口都会报"权重还没解包"、然后被标成"本次不可用"——那是误导。
        enabled = {
            settings.value.recordBehavior && settings.value.coverVectors && settings.value.embedNpu
        },
        foreground = { foreground.isForeground },
        // 纯文字优先：它在算 / 队列里还有东西时封面等着（编图要求"手头没有批在算"）
        textIdle = { !embedding.busy },
        log = logLine,
        onWindow = { w ->
            perf?.eventCoverWindow(w.count, w.visionMs, w.textMs, w.totalMs, w.compiled)
        },
    )

    /**
     * 界面用的翻译门面：卡片 / 正文 / 评论都从这里拿（[feedhub.app.translate.LocalTranslation]）。
     * **引擎是懒建的**（工厂只在第一次真的要翻时才调用）：没有任何点亮平台的内容时连引擎都不建，
     * 更不会启动 llama-server。
     */
    val translation: TranslationStore = TranslationStore(
        scope,
        engineFactory = translationEngineFactory?.let { factory -> { factory(settings) } },
        settings = settings,
        cacheDir = translationCacheDir,
        identifier = translationIdentifier,
    )

    /**
     * 按平台查代理（每个请求现读）：「内置核心」那一档现问 [proxyCore]（在跑给本机 SOCKS5 端口，
     * 没在跑给一个 `valid == false` 的替身 → 请求直接失败，绝不悄悄直连）。
     *
     * 「代理不可用时禁止直连」（[AppSettings.proxyExitLock]）**关着**时，代理用不了就不再报错，
     * 点亮的平台改成直连；第一次落到这种状态时往 [logLine] 写一行"出口锁关闭，直连"（不含地址），
     * 让"说好的走代理结果直连了"这件事留痕（见 [SettingsStore.proxyProvider]）。
     */
    private val proxyProvider: ProxyProvider = settings.proxyProvider(
        builtin = { proxyCore?.proxyMode() ?: NO_BUILTIN_CORE },
        awaitBuiltin = { proxyCore?.awaitStarting() },
        onDirectFallback = { reason -> logLine("[代理] 出口锁关闭，直连（$reason）；「代理不可用时禁止直连」在设置里关着") },
    )

    /**
     * 网页登录（安卓的系统 WebView）打开页面前的代理结论。
     *
     * 和接口 / 图片用的是**同一份**判定（[proxyProvider]：设置里的代理 + 这个平台在不在「走代理的平台」里），
     * 不在这里另列平台名单；境外平台（插件描述的 `overseas`）只要不是"确实有代理"就拒绝加载，
     * 见 [feedhub.app.login.WebLoginProxyRules]。安卓那边把它交给 `ProxyController`。
     */
    fun webLoginProxy(p: Platform): WebLoginProxyPlan =
        WebLoginProxyRules.plan(proxyProvider.proxyFor(p), overseas = platforms[p]?.overseas == true)

    /**
     * 这个平台现在**因为代理用不了而一个请求都不该发**时的原因；null = 可以发。
     *
     * 和请求真正被拦下时用的是**同一份判定**（[AppSettings.proxyFor] + [feedhub.core.ProxyMode.unavailableReason]，
     * 也就是 [feedhub.core.ProxyConfigGuardInterceptor] 那一道），只是提前问一次——桌面 App 的对外接口用它
     * 在发请求之前就把 `proxy_unavailable` 报出来（口径和 `cli/CliProxy.kt` 的 fail-closed 一样）。
     */
    fun proxyBlockedReason(p: Platform): String? =
        ProxyRules.unavailableReason(settings.value.proxyFor(p, proxyCore?.proxyMode() ?: NO_BUILTIN_CORE, platforms))

    /**
     * 这个进程从启动到现在向平台发出去的 HTTP 请求数（只数**平台接口**的请求，图片 / 验证码浏览器不算）。
     *
     * 进程级的总量（诊断 / 测试用）。**对外接口的 `requests` 不用它**：那是按调用算的
     * （[feedhub.core.RequestScope]），App 自己同时取流不会把 agent 的数撑大。
     */
    fun httpRequests(): Long = httpCalls.get()

    /**
     * 各页面用的 [SourceContext]：和传进来的那份共用 Cookie 罐 / 连接池，只是把代理换成设置里的
     * （[proxyProvider] 每次请求现读设置，所以改代理不用重建 Source）。
     * 各 Source 用 [SourceContext.client] 派生客户端，于是接口请求都跟着这个平台的代理走。
     *
     * 代理设置变了会整份换掉（新的连接池 + 重建 Source），见 [watchProxySettings]。
     */
    @Volatile // resetProxyClients() 在 Compose 主线程换掉它，别的线程（Coil 的 IO 线程等）会读
    var ctx: SourceContext = baseContext.withProxy(proxyProvider, captcha)
        .withRequestObserver {
            httpCalls.incrementAndGet()
            // 同步发送（Call.execute()）没有"发起"那个可挂的协程点，靠拦截器在**同一条线程**上记：
            // 对外接口的调用会把这个 scope 放进线程局部变量（见 RequestScope），App 自己取流时是 null。
            feedhub.core.RequestScope.currentThread()?.record()
        }
        private set

    /** 发出去的平台请求数（[httpRequests] 读它；[ctx] 的 [SourceContext.onRequest] 加）。 */
    private val httpCalls = java.util.concurrent.atomic.AtomicLong()

    /**
     * 域名后缀 → 平台：**不建 Source 就能读**（[hosts] 的静态表）。
     * 以前这里要问每个 Source 的 `hosts`，等于把 Source 都建出来。
     */
    private val imageHostTable: Map<String, Platform> = platformHostTable(hosts.filterKeys { it in sources.keys })

    /**
     * **取图**。
     *
     * 两件事在这里合成一份：
     * 1. [imageFetch]：真发请求的那一层（[imageFetchBase]，产品路径是 `RustImageFetch`）；
     * 2. 浏览器 UA（`UserAgents.DESKTOP_CHROME`，酷安的图片 CDN 对没有浏览器 UA 的请求回 567）。
     *
     * 代理**逐请求**算（[imageProxy]：按图片地址的域名认出是哪家的），所以代理设置变了**不用换这个对象**——
     * 它每次请求都现读设置（和 `PlatformProxySelector` 同一个行为）。
     * Coil 每次请求现读它（`App.kt` 的 `appImageLoader`）。
     */
    val imageFetch: ImageFetch = DefaultHeadersImageFetch(
        delegate = imageFetchBase?.withProxy(::imageProxyMode)
            ?: feedhub.app.image.RustImageFetch(proxyFor = ::imageProxy),
        headers = mapOf("User-Agent" to UserAgents.DESKTOP_CHROME),
    )

    /**
     * 一张图走哪一档代理：按域名认出平台就用那个平台的配置，认不出跟随系统（[ProxyMode.System]）。
     *
     * 和 `PlatformProxySelector.select` 逐条对齐：
     * - `Direct` → 直连；
     * - `Custom` 没填全 / `BuiltinCore` 没在跑 → [ProxyTarget.Unavailable]（**fail-closed，一个字节都不发**，
     *   文案就是 `invalidCustomProxy` / `builtinCoreNotRunning`）；
     * - `System` → [ProxyTarget.of] 问 JVM 的选择器（解析不出来是 `Unavailable`，不是直连）。
     *
     * 宿主把域名表换成 `hosts` 的几个域时（演示 / 测试），这张表也跟着换——和接口那条路同一份判定。
     */
    private fun imageProxy(host: String): ProxyTarget {
        // `ProxyMode.System` 要拿一个域名去问选择器：认得出平台就用它的主域名（口径同平台对象那条），
        // 认不出就用这张图自己的域名（就是 `PlatformProxySelector` 拿到的那个 uri.host）。
        val hosts = imageHostTable.platformForHost(host)?.let { hosts[it] }?.takeIf { it.isNotEmpty() } ?: listOf(host)
        return ProxyTarget.of(imageProxyMode(host), hosts)
    }

    /**
     * **视频分片中转**。
     *
     * 播放器要一个"把远端地址换成本机回环"的服务，它只连本机；真正去 CDN 取分片的是 Rust 那边
     * （`feedhub-ffi/src/relay_server.rs`）用**这个平台当下那一档代理**发的。所以工厂在这里：
     * 代理（[proxyProvider]）和拉流头（`PlayInfo.headers`）都只有 `AppGraph` 知道。
     *
     * 每次调用造**一个新的服务**（内核给新端口）：换地址 / 换清晰度 / 重开播放都该有一份干净的登记表，
     * 用完由引擎 `close()`（在飞的上游请求一起取消）。
     *
     * 代理口径和接口 / 图片**逐条一样**（[ProxyTarget.of] 按平台的主域名问一次系统代理，解析不出来的
     * 那一档是 `Unavailable`，也就是 fail-closed：那个平台一个字节都不发）。
     */
    fun relay(platform: Platform, headers: Map<String, String> = emptyMap()): RelayFactory = RelayFactory {
        feedhub.app.video.createRelay(platform, platformProxy(platform), headers)
    }

    /** 这个平台（按平台 id）走哪一档代理——和 [imageProxy] 共用同一份判定，只是域名是平台的主域名。 */
    private fun platformProxy(platform: Platform): ProxyTarget {
        val hosts = hosts[platform].orEmpty()
        return ProxyTarget.of(proxyProvider.proxyFor(platform), hosts)
    }

    /** 这张图（按域名）走哪一档：认得出平台就用那个平台的配置，认不出跟随系统。 */
    private fun imageProxyMode(host: String): ProxyMode =
        imageHostTable.platformForHost(host)?.let(proxyProvider::proxyFor) ?: ProxyMode.System

    private val made = mutableMapOf<Platform, Source>()

    fun source(p: Platform): Source = synchronized(made) {
        made.getOrPut(p) { sources.getValue(p)(ctx) }
    }

    /**
     * 同 [source]，但这个平台在这里用不了（插件没装——老收藏 / 历史里留下的条目；或者这一端不支持）时返回 null，
     * 不抛异常。打开详情这类"条目来自存储"的入口用它。
     */
    fun sourceOrNull(p: Platform): Source? = if (available(p)) source(p) else null

    /**
     * 这个平台在这台机器上能不能用（[unavailable] 里的不算）。设置页画平台标签时读它——
     * **不构造 Source**。
     */
    fun available(p: Platform): Boolean = p !in unavailable && p in sources.keys

    /**
     * 这个平台能不能请求这个流：[available] 且它的 Source 支持 [kind]。
     * 不可用的平台直接 false（不去构造），所以设置页可以放心地按 [platforms] 画一排标签。
     */
    fun supports(p: Platform, kind: FeedKind): Boolean = available(p) && kind in source(p).feeds

    /** "我的"页的状态（选中的入口、筛选条件、登录状态、关注列表），详情全屏时"我的"会离开组合，所以放在这里。 */
    val mine = MineState(scope, sources.keys.toList(), ::source, ctx.cookies, cookieImporter, webLogin, ctx.store)

    val home = FeedState(
        FeedKind.RECOMMEND, scope, { feedSources(FeedKind.RECOMMEND) }, { settings.value.contentType },
        { settings.value.contentFilter }, maxPerPlatform = ::maxPerPlatform,
        // 首页排序：穿插（默认）/ 按时间（老行为），设置里切；关注页不受它影响（见 BatchOrders）
        order = { settings.value.homeOrder },
        snapshot = feedSnapshot(FeedKind.RECOMMEND), log = logLine,
        foreground = { foreground.isForeground },
        // 封面向量：同一批新卡片也排一份（只在充电时、一批一个窗口地算，见 CoverVectorScheduler）
        onNewContent = { embedding.offer(it); coverVectors.offer(it) },
        // 刷信息流时平台报"没登录"：把「我的 → 账号」里这个平台标成过期（不发请求，见 AccountsState.markExpired）
        onAuthExpired = { mine.accounts.markExpired(it) },
    )
    val following = FeedState(
        FeedKind.FOLLOWING, scope, { feedSources(FeedKind.FOLLOWING) }, { settings.value.contentType },
        { settings.value.contentFilter }, maxPerPlatform = ::maxPerPlatform,
        snapshot = feedSnapshot(FeedKind.FOLLOWING), log = logLine,
        foreground = { foreground.isForeground },
        // 封面向量：同一批新卡片也排一份（只在充电时、一批一个窗口地算，见 CoverVectorScheduler）
        onNewContent = { embedding.offer(it); coverVectors.offer(it) },
        onAuthExpired = { mine.accounts.markExpired(it) },
    )

    /**
     * 跨平台搜索：混排规则在 [feedhub.app.search.SearchState]，
     * 搜索页在 `app/search/SearchScreen.kt`。搜索**只发一次第一页 / 平台**，翻页各家最多 3 页。
     */
    val search = feedhub.app.search.SearchState(
        scope,
        { searchSources() },
        { settings.value.contentFilter },
        foreground = { foreground.isForeground },
        log = logLine,
    )

    /**
     * 演示 / 截图用：把搜索平台换成不联网的假 Source（`fake=` 的演示环境 + `query=` 截图）。
     * null = 用真实插件（默认）。设了之后 [searchPlatforms] 也跟着它走。
     */
    var searchOverride: (() -> List<feedhub.core.SearchSource>)? = null

    /**
     * 这次能搜哪些平台（不构造 Source，界面用它写搜索框的占位文字）。
     *
     * 规则在 core 的 [feedhub.brain.SearchPlan.platforms]（cli 的 `search all` 用的是同一份）：
     * 首页点亮 + 声明了搜索能力 + 这一端能用，顺序 = `plugins.toml` 的顺序。
     */
    fun searchPlatforms(): List<Platform> =
        searchOverride?.invoke()?.map { it.platform }
            ?: feedhub.brain.SearchPlan.platforms(platforms, settings.value.homePlatforms, availablePlatforms())

    /** 搜索请求哪些平台：同上，再把平台换成 Source。**不按平台 id 分支**，也不构造不用的 Source。 */
    private fun searchSources(): List<feedhub.core.SearchSource> =
        searchOverride?.invoke()
            ?: searchPlatforms().mapNotNull { source(it) as? feedhub.core.SearchSource }

    /** 这一端真能用的平台（装了、跑得了、没被标成暂不可用）。不构造 Source。 */
    private fun availablePlatforms(): Set<Platform> = platforms.platforms.filterTo(LinkedHashSet()) { available(it) }
    /** 设置里的「每平台一批最多」（0 = 不限）；每批现读，改了下一批就生效。 */
    private fun maxPerPlatform(): Int = settings.value.maxPerPlatform

    private fun feedSnapshot(kind: FeedKind) = feedSnapshotDir?.let { FeedSnapshot(it.resolve(FeedSnapshot.fileName(kind))) }

    /**
     * [watchFeedSettings] 盯的那几项（变了就把首页 / 关注刷新一遍）。用 data class 而不是 Triple：
     * 现在有五项（内容类型、两排平台、内容过滤、每平台最多几条、**首页排序**）。
     */
    private data class FeedSettingsKey(
        val contentType: ContentType,
        val platforms: Pair<Set<Platform>, Set<Platform>>,
        val filter: ContentFilter.Rules,
        val maxPerPlatform: Int,
        /** 用户马上能看到新顺序（顺序只对新的一批生效）。 */
        val homeOrder: FeedOrder,
    )

    /**
     * [watchProxySettings] 盯的那几项（变了就换一份客户端）：全局代理、走代理的平台、
     * **按平台指定代理**、内置核心的状态。
     */
    private data class ProxySettingsKey(
        val proxy: ProxyConfig,
        val proxied: Set<Platform>,
        val perPlatform: Map<Platform, feedhub.core.ProxyChoice>,
        val core: CoreStatus?,
    )

    /**
     * 按设置里"历史记录只保留最近 N 天"清理历史；没开就什么也不做。返回清掉的条数。
     * 启动时和设置页改了这组选项之后调。
     */
    suspend fun applyHistoryRetention(): Int {
        val s = settings.value
        if (!s.historyLimitOn) return 0
        val cutoff = System.currentTimeMillis() - s.historyDays * 86_400_000L
        return withContext(Dispatchers.IO) { library.pruneHistory(cutoff) }
    }

    /**
     * 行为数据只在启动时清一次。两样分开算：
     * - 曝光 / 停留（`exposure`）：保留 [EXPOSURE_RETENTION_DAYS] 天（90，一行才 101 字节）；
     * - 曝光过的卡片内容（`seen_item`）：单独保留 [SEEN_ITEM_RETENTION_DAYS] 天。
     *
     * 返回清掉的曝光条数。
     */
    suspend fun applyBehaviorRetention(): Int {
        val nowMs = System.currentTimeMillis()
        val cutoff = nowMs - EXPOSURE_RETENTION_DAYS * 86_400_000L
        return withContext(Dispatchers.IO) {
            val pruned = library.pruneExposure(cutoff)
            // 曝光过的卡片内容单独按 30 天清（比曝光短：一行带整段 item_json，90 天可能到上百 MB）
            library.pruneSeenItems(seenItemCutoff(nowMs))
            // 卡片向量跟着 seen_item 一起清：它们只对"还留着的那些卡片"有意义
            library.pruneEmbeddings(seenItemCutoff(nowMs))
            pruned
        }
    }

    /**
     * 某个流这次要请求的平台：设置里勾选的、支持这个流的；内容类型的名单见 [requestedPlatforms]
     */
    private fun feedSources(kind: FeedKind): List<Source> {
        val s = settings.value
        val enabled = if (kind == FeedKind.RECOMMEND) s.homePlatforms else s.followingPlatforms
        val wanted = requestedPlatforms(kind, s.contentType, enabled, platforms)
        return sources.keys.filter { it in wanted }
            .map(::source)
            .filter { kind in it.feeds }
    }

    /**
     * 空状态判定用的事实：首页 / 关注为什么空。
     * 界面上只有一处组装（`App.kt` 的 `TabContent`），判定本身是纯函数 [feedhub.app.feed.feedEmptyFor]。
     *
     * **不构造 Source**（和 [available] / [searchPlatforms] 一条规矩）：
     * "谁有这个流"看插件描述，"登录了没有"看账号页那份状态 + Cookie 罐里有没有那几个键。
     * 每帧读它也没有副作用。
     */
    fun feedEmptyInputs(
        kind: FeedKind,
        errors: Map<Platform, String> = emptyMap(),
        refused: Set<Platform> = emptySet(),
    ): feedhub.app.feed.FeedEmptyInputs {
        val s = settings.value
        val enabled = if (kind == FeedKind.RECOMMEND) s.homePlatforms else s.followingPlatforms
        val lit = platforms.platforms.filter { it in enabled && available(it) }
        // 支持这个流的：插件描述说了算
        val supported = lit.filterTo(LinkedHashSet()) { platforms[it]?.capabilities?.feeds?.contains(kind) == true }
        // 内容类型会在请求之前筛掉一些平台（和 feedSources 用同一个函数，免得两边说法不一致）
        val requested = requestedPlatforms(kind, s.contentType, supported, platforms)
        val loggedOut = LinkedHashSet<Platform>()
        for (p in lit) if (loginStateOf(p) == false) loggedOut += p
        return feedhub.app.feed.FeedEmptyInputs(
            kind = kind,
            contentType = s.contentType,
            lit = lit,
            supportsFeed = supported,
            droppedByContentType = supported - requested,
            requested = requested,
            loggedOut = loggedOut,
            refused = refused,
            errors = errors,
        )
    }

    /**
     * 这个平台登录了没有：账号页查到的状态优先（含 YouTube 那种 token 登录），没查过就看插件描述的
     * `requiredCookies` 在 Cookie 罐里齐不齐。**判不出来返回 null**（不猜：宁可说"没内容"，也别乱叫人去登录）。
     */
    private fun loginStateOf(p: Platform): Boolean? {
        when (mine.accounts.status[p]) {
            is feedhub.app.mine.AccountStatus.LoggedIn -> return true
            is feedhub.app.mine.AccountStatus.LoggedOut -> return false
            else -> Unit
        }
        val spec = platforms[p]?.login?.web ?: return null
        if (spec.requiredCookies.isEmpty()) return null
        return mine.accounts.hasLoginCookies(spec)
    }

    init {
        current = this
        // 开启 Cookie 落盘 2 秒防抖（E4）：刷信息流时十几次 Set-Cookie 合并为一次写盘 + fsync，
        // 退后台（onStop）或退出（shutdown）时立即 flush。
        feedhub.cookies.enableCookieJarDebounce(baseContext.cookies)
        // 性能记录：把开关和前后台接上。**开关关着时 [feedhub.app.perf.PerfRecorder] 一个定时器都不起**
        // （它自己的 setEnabled / setForeground 管这个）；全局打点口子（PerfHooks）也在这里挂上。
        perf?.let { p ->
            feedhub.app.perf.PerfHooks.attach(p)
            foreground.addListener { p.setForeground(it) }
            scope.launch { snapshotFlow { settings.value.perfLog }.collect { on -> p.setEnabled(on) } }
        }
        // 本地向量走 NPU：开关切换**不用重启 App**。接上初始值，之后设置页每改一次就推一次；
        // 没有 NPU 路由（桌面 / 没带 NPU 的包）时这里什么都不做。
        // 所以这里只剩开关。
        embeddingBackend?.let { ctl ->
            ctl.setRequested(settings.value.embedNpu)
            scope.launch {
                snapshotFlow { settings.value.embedNpu }.collect { on -> ctl.setRequested(on) }
            }
        }
        // 对外接口：**开关打开时起、关掉时停并删 socket**。初始值也走一次，
        // 所以 App 启动时开关是开着的就直接在听。起停本身很快（bind 一个 unix socket），跑在 IO 上不占主线程。
        appApi?.let { api ->
            scope.launch {
                snapshotFlow { settings.value.appApi }.collect { on ->
                    withContext(Dispatchers.IO) { if (on) api.start() else api.stop() }
                }
            }
        }
        watchFeedSettings()
        watchProxySettings()
        watchProxyCore()
        scope.launch(Dispatchers.IO) {
            applyHistoryRetention()
            applyBehaviorRetention()
            // 收藏 / 历史补算：清完保留期再排队，免得给马上要被删的行算。
            // 只在有向量模型、开关开着、在前台时才会真的动（`EmbeddingScheduler` 自己挡）。
            withContext(Dispatchers.Main) {
                embedding.requestBackfill()
            }
        }
        // 「记录浏览行为」从关变开（或这个开关第一次被读到）时，把收藏 / 历史补算叫醒。
        scope.launch {
            snapshotFlow { settings.value.recordBehavior }.collect { on -> if (on) embedding.requestBackfill() }
        }
    }

    /**
     * App 进 / 退前台（安卓 `MainActivity` / `DebugStartActivity` 的 `onStart` / `onStop` 调；桌面不调）。
     *
     * 退到后台：**立刻**停掉一切预取 / 刷新（不是在下次组合时才发现）、刷新率请求撤回。国产 ROM 会冻结第三方 App、
     * 深度睡眠还会整机断网，后台继续取既取不到也费电。
     *
     * 回到前台：首页 / 关注按"离上次取到新内容多久"决定要不要**只预取**，见 [FeedState.onForeground]；
     * 刷新率重新要最高。
     *
     * 重复的 onStart / onStop 不会重复动作（[AppForeground.set] 返回 false 时直接返回）。
     */
    fun onForeground(active: Boolean) {
        if (!foreground.set(active)) return
        frameRate.setForeground(active)
        // 向量：退到后台立刻取消（正在算的那批结果丢掉、不写库），并把会话释放掉（约 300 MB，
        // 下次回前台第一次要算时再懒建）；回前台刷新一下"已算条数"
        embeddingBackend?.onForeground(active)
        embedding.onForeground(active)
        coverVectors.onForeground(active)
        // 封面的两张会话常驻，也只在前台常驻：退后台和纯文字一起关，回前台第一个窗口再建
        if (!active) coverEmbedder?.close()
        if (!active) embedder?.close()
        // 翻译：安卓那条在退后台时**立刻**停 llama-server
        // （桌面那条是空实现，行为不变）。引擎还没建出来时这是空操作。
        translation.onForeground(active)
        if (active) {
            embedding.refreshCountAsync()
            // 回前台：补算接着排（退后台时队列被取消了，正在算的那批也丢了）
            embedding.requestBackfill()
            home.onForeground()
            following.onForeground()
            keepLoginSessionsFresh()
        } else {
            pausePlayback() // onPause 一般已经停过了，这里兜底
            home.onBackground()
            following.onBackground()
            // 退到后台：把防抖中的设置、Cookie 与异步资料库写队列在后台线程刷盘，避免进程在后台被冻结/回收时丢状态。
            scope.launch(Dispatchers.IO) {
                runCatching { settings.flush() }
                runCatching { library.flush() }
                runCatching { feedhub.cookies.flushCookieJar(ctx.cookies) }
            }
        }
    }

    /**
     * 回前台时让设备码登录的平台（YouTube）把过期的 access token 先换好：它只活 1 小时，
     * 不先换的话第一次点播放要多等一跳。只看**已经建过**的 Source（冷启动时首页取流会自己换），没过期就一个请求都不发。
     */
    private fun keepLoginSessionsFresh() {
        val sessions = synchronized(made) { made.values.toList() }.filterIsInstance<DeviceCodeLoginSource>()
        for (s in sessions) {
            scope.launch(Dispatchers.IO) { runCatching { s.keepSessionFresh() } }
        }
    }

    /**
     * 打开了一条内容的详情：顺手把它排进"算向量"的队列。
     *
     * 点开过的条目可能是**从历史 / 收藏里进来的**（不在首页那一批里），这条通路保证它们也算得上
     * 。18+ 不记历史、这里也不排（[EmbeddingScheduler.offer]
     * 里还有一道 [feedhub.app.embed.hasEmbeddingText]）。
     */
    fun onItemOpened(item: FeedItem) {
        embedding.offer(listOf(item))
        // 封面向量也跟着排（平台在 CoverRules.platformTable 里、有封面图的才算）
        coverVectors.offer(listOf(item))
    }

    /**
     * 详情正文**已经加载出来**：用"标题 + 正文开头"重算一份，覆盖这条的向量。
     *
     * 只在详情页真的拿到正文之后调（[PostDetailScreen] / [feedhub.app.video.VideoDetailScreen]）；**不为了向量
     * 去多发一个请求**——首页 / 关注里的卡片永远只用卡片自己带的文字。
     *
     * [body] 是正文的纯文本（[feedhub.core.RichText.textOnly]，不含 `[图片]` 这种占位符）；空的（纯图片帖）就退回
     * 卡片文字。正文**只用来算向量**：不存库、不进导出。
     */
    fun onDetailLoaded(item: FeedItem, body: String?) {
        embedding.offerDetail(item, body)
    }

    /**
     * 把所有在播的视频暂停。不做后台播放：安卓在 `Activity.onPause` 就调
     * （一按电源键就触发，比 `onStop` 早半秒多，声音几乎立刻断）。切走前在播的，回前台由 [resumePlayback] 接着播。
     */
    fun pausePlayback() {
        for (session in feedhub.app.video.ActivePlaybackSessions.alive()) runCatching { session.pauseForBackground() }
    }

    /** 回到前台（安卓 `Activity.onResume`）：切走前在播的会话从原位置续播，用户自己暂停的不动。 */
    fun resumePlayback() {
        for (session in feedhub.app.video.ActivePlaybackSessions.alive()) runCatching { session.resumeFromBackground() }
    }

    /**
     * 代理设置改了：等 [proxyChangeDebounceMs] 没有新改动（设置页是边打字边存的）之后，**换掉整份客户端**。
     *
     * 代理虽然是每次请求现查的（[feedhub.core.PlatformProxySelector]），但 OkHttp 用 ProxySelector 时
     * 选出来的代理**不进** `Address`，连接池对"主机名相同"的连接照样判为可用，于是改完设置后那台主机上
     * 已经建好的连接会被接着用、走旧代理（[ConnectionPool.evictAll] 也只能清**空闲**的连接，
     * 正在用的清不掉）。所以这里换一个新的连接池：旧连接留给旧客户端（在飞的请求跑完自然回收），
     * 新请求一律走新池；Source 缓存也清掉，它们持有的是旧客户端。
     */
    @OptIn(FlowPreview::class)
    private fun watchProxySettings() {
        scope.launch {
            // 内置核心的端口 / 有没有在跑也要看：变了必须换客户端（在跑 → 新端口，挂了 → 立刻 fail-closed）。
            // 它那条主机上
            // 已经建好的连接也会被旧连接池接着用，所以一并观察。
            snapshotFlow {
                ProxySettingsKey(
                    proxy = settings.value.proxy,
                    proxied = settings.value.proxiedPlatforms,
                    perPlatform = settings.value.platformProxy,
                    core = proxyCore?.status?.value,
                )
            }
                .drop(1)
                .debounce(proxyChangeDebounceMs)
                .collect { resetProxyClients() }
        }
    }

    /**
     * 内置核心的状态变了（起来了 / 挂了 / 换了端口）：**也要换一份客户端**。
     *
     * 理由和改代理设置一样：核心的端口进了 SOCKS5 替身地址，旧连接池里"同一个主机名"
     * 的连接会被接着用；而且核心从"没跑"变成"在跑"（或反过来）时，走代理的平台必须立刻改用新端口 /
     * 立刻 fail-closed。所以把 [CoreStatus] 也放进 [watchProxySettings] 的观察里。
     */
    /**
     * App 进前台 / 退到后台（安卓 `MainActivity` 的 `onStart` / `onStop` 喂；桌面没有这一步）。
     *
     * 判定在 Rust 里（前台 + 选了「内置核心」→ 起；退后台 30 秒宽限）；这里只按它的答复起停。
     * 没有核心的组装什么也不做。
     */
    fun applyForeground(foreground: Boolean) {
        val core = proxyCore ?: return
        if (!coreForeground) return
        core.setForeground(foreground)?.let { core.setEnabled(it) }
    }

    private fun watchProxyCore() {
        val core = proxyCore ?: return
        // 设置里选了「内置核心」才起它；选了别的就停掉（不占内存）。
        // 安卓还多一层"只在前台跑"：先喂给 [coreForeground]（前台判定 + 30 秒宽限），由它去调 setEnabled。
        val gated = coreForeground
        scope.launch {
            snapshotFlow { settings.value.proxy.mode }
                .collect { mode ->
                    val wanted = mode == ProxyConfig.Mode.BUILTIN
                    // 安卓：先过"只在前台跑"的判定（它会给一个"现在该不该跑"）；桌面直接起停
                    if (gated) {
                        core.setEnabledGate(wanted)?.let { core.setEnabled(it) }
                    } else {
                        core.setEnabled(wanted)
                    }
                }
        }
    }

    /**
     * 换一份基础客户端（新的连接池）并重建 Source 缓存；图片客户端也跟着换。
     *
     * 清缓存这一步对 **Rust 联网层**（`RustNetStack`）是必须的：那一层在**建平台对象时**就把代理解析成了
     * 具体的一档（`ProxyTarget.of`，`ProxyMode.System` 要拿平台主域名去问 JVM 的 `ProxySelector`），
     * 不像 OkHttp 那边每个请求现问 `PlatformProxySelector`。不清缓存的话平台对象、连同它的 `FfiNetStack`
     * 会一直用旧代理。清掉之后下一次 [source] 会用新的 [ctx] 重造，顺带重新问一次系统代理。
     */
    private fun resetProxyClients() {
        ctx = ctx.withFreshConnectionPool()
        synchronized(made) { made.clear() }
        // 图片那边**没有要换的东西**（代理逐请求现读设置，见 [imageProxy]），这里只留一行说明。
    }

    /**
     * 设置页改了内容类型 / 平台 / 内容过滤名单 / 每平台最多几条后，等 1.5 秒没有新的改动（连着点几个平台只刷一次）
     * 就把首页和关注页刷新一遍。**每平台最多也算**：一批是按旧的上限截的，改了就该按新的重取一次，
     * 不然"点了 8 却还是 15 条"要等下一次刷新（预取里已经只有 8～15 条，补不回来）。
     *
     * 内容过滤比另外两项多一步：[FeedState.dropHidden] 先把屏幕上已经显示、按新规则该藏掉的去掉，
     * 再刷新。反方向解除不了：之前藏掉的条目要等这一批新内容回来才重新出现。
     */
    @OptIn(FlowPreview::class)
    private fun watchFeedSettings() {
        scope.launch {
            snapshotFlow {
                FeedSettingsKey(
                    contentType = settings.value.contentType,
                    platforms = settings.value.homePlatforms to settings.value.followingPlatforms,
                    filter = settings.value.contentFilter,
                    maxPerPlatform = settings.value.maxPerPlatform,
                    homeOrder = settings.value.homeOrder,
                )
            }
                .drop(1)
                .debounce(1500)
                .collect {
                    // 设置变了：预取的那一批是按旧设置取的，一定重新请求
                    home.dropHidden()
                    following.dropHidden()
                    home.refresh(fresh = true)
                    following.refresh(fresh = true)
                }
        }
    }

    /**
     * 关闭窗口时的收尾。以前 [Main] 里直接 `exitProcess(0)`：那会跳过 Compose 的
     * `onDispose`，正在播的视频不会存进度、设置里还在防抖的那次改动会丢、翻译缓存的写线程被硬切。
     *
     * 这里**同步**做三件事（每件都在自己的线程上跑，超时不等）：
     * 1. 活着的播放会话存进度 + 释放播放器（[ActivePlaybackSessions.releaseAll]）；
     * 2. 设置立刻落盘（[SettingsStore.flush]，含还在防抖里的那次）；
     * 3. 翻译：排队中 / 还没落盘的缓存写完、llama-server 停掉（[TranslationStore.close]）。
     * 还有内置代理核心（sing-box 子进程）也顺手停掉，别留孤儿进程。
     *
     * **有总时间上限**（默认 3 秒）：某一步卡住（磁盘 / 进程不响应）也不能让窗口关不掉；到点就走，
     * 没做完的步骤记一笔日志。
     */
    fun shutdown(timeoutMs: Long = 3_000L) {
        val deadline = System.currentTimeMillis() + timeoutMs
        fun step(name: String, block: () -> Unit) {
            val left = deadline - System.currentTimeMillis()
            if (left <= 0) {
                System.err.println("feedhub 收尾：时间用完，跳过 $name")
                return
            }
            val thread = Thread(block, "feedhub-shutdown-$name").apply { isDaemon = true }
            thread.start()
            runCatching { thread.join(left) }
            if (thread.isAlive) System.err.println("feedhub 收尾：$name 超过 ${timeoutMs}ms 还没完，不等了")
        }
        step("perf") { perf?.close() }
        step("api") { appApi?.close() }
        // 封面向量：先把它当前停掉、两张 QNN 会话关掉，再收纯文字那边——
        // 反过来的话封面窗口可能正拿着"已被关掉的纯文字会话"在跑（分时是串行的，得按同一顺序退）。
        step("cover") {
            coverVectors.cancel()
            coverEmbedder?.close()
        }
        step("embedding") { embedder?.close() }
        step("playback") { ActivePlaybackSessions.releaseAll() }
        step("feeds") {
            runCatching { home.close() }
            runCatching { following.close() }
        }
        step("settings") { settings.flush() }
        step("cookies") { feedhub.cookies.flushCookieJar(ctx.cookies) }
        step("library") {
            library.flush()
            (library as? AutoCloseable)?.close()
        }
        step("translation") { translation.close() }
        step("proxy-core") { proxyCore?.close() }
    }

    companion object {
        /**
         * 当前进程里**最后建好**的那份 graph。
         *
         * 给 [feedhub.app.feed.LocalCardGraph] 兜底：卡片长按菜单要收藏 / 屏蔽 / 分享，而卡片在
         * 首页 / 关注 / 收藏 / 历史四处画，那几处的调用点当前不动，没法把 graph 一路传进 `FeedCard`。
         * 一个进程只有一份 App，和 [feedhub.app.video.ActivePlaybackSessions] 是一样的做法；
         * 测试 / 截图可以照常用 `CompositionLocalProvider(LocalCardGraph provides …)` 覆盖掉。
         */
        var current: AppGraph? = null
            private set
    }
}
