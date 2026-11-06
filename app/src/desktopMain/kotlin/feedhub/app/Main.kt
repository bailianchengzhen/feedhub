package feedhub.app

import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.SideEffect
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.unit.Density
import androidx.compose.ui.unit.dp
import androidx.compose.ui.window.Window
import androidx.compose.ui.window.application
import androidx.compose.ui.window.rememberWindowState
import feedhub.app.captcha.CaptchaHelperLauncher
import feedhub.app.captcha.CaptchaPromptState
import feedhub.app.captcha.DesktopCaptchaHandler
import feedhub.app.library.Library
import feedhub.app.library.MemoryLibrary
import feedhub.app.library.RustLibrary
import feedhub.app.login.ChromeCookieImporter
import feedhub.app.ui.FontWarmup
import feedhub.app.feed.LocalPluggedIn
import feedhub.app.video.LocalWindowVisible
import feedhub.app.translate.BergamotModelFiles
import feedhub.app.translate.DesktopTranslateRuntime
import feedhub.app.translate.createDesktopTranslationEngine
import feedhub.app.video.DecodeLedger
import feedhub.app.video.decodeEnvironmentFingerprint
import feedhub.core.ProxyMode
import feedhub.core.SourceContext
import feedhub.core.plugin.PluginTarget
import feedhub.plugins.InstalledPlugins
import feedhub.core.ItemKind
import feedhub.core.DetailTarget
import feedhub.core.FeedItem
import feedhub.core.local.LocalFiles
import feedhub.proxy.ProxyCore
import feedhub.rust.ProxyTarget
import feedhub.proxy.ProxyCoreBridge
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import java.nio.file.Path
import java.nio.file.Paths
import kotlin.math.abs
import kotlin.system.exitProcess

/**
 * 桌面版组装：Cookie 和状态用 cli 同一套本地文件（~/.config/feedhub），本地资料库（收藏 / 历史 / 屏蔽）是同目录下的
 * `library.db`，图片缓存放 ~/.cache/feedhub/images。[library] 截图工具会换成临时的（别往用户真实的库里写）。
 *
 * 内置代理核心（sing-box 子进程）也在这里组装：
 * - 二进制按 [ProxyCoreBridge.locate] 找（打包后是 App 的 resources 目录，也可以用环境变量 `FEEDHUB_SINGBOX` 指定），
 *   找不到也能用 App，只是「内置核心」这一档会 fail-closed；
 * - 机密（订阅地址、节点密码）存 `~/.config/feedhub/proxy-core.json`（权限 600），
 *   生成的 sing-box 配置放 `~/.config/feedhub/singbox/`（也是 600）。
 *
 * @param proxyCoreOverride 测试 / 截图用的替换（比如指向夹具二进制、或者根本不要核心）。
 * @param proxyStateFile 机密状态文件；不给就用用户的那一份。截图工具会换成临时目录里的。
 * @param proxyDataDir 生成的 sing-box 配置放哪；不给就是 `~/.config/feedhub/singbox/`。
 */
fun desktopGraph(
    library: Library = openLibrary(),
    ctx: SourceContext = LocalFiles.context(),
    proxyCoreOverride: ProxyCore? = null,
    proxyStateFile: Path? = null,
    proxyDataDir: Path? = null,
    withProxyCore: Boolean = true,
    deviceKeys: feedhub.core.sync.DeviceKeyStore? = feedhub.core.sync.DeviceKeyStore(LocalFiles.home),
    /** 对外接口的 socket 放哪；不给就是 [feedhub.app.api.DesktopAppApi.defaultApiSocketPath]（测试 / 截图传临时目录）。 */
    apiSocketPath: Path? = null,
    /** 截图工具用：换成不 bind socket 的假接口（[feedhub.app.dev.FakeAppApi]）；null = 真的那份。 */
    appApiOverride: feedhub.app.api.AppApi? = null,
    /**
     * 首页 / 关注"上一批"的快照目录（默认 `~/.cache/feedhub`）。截图工具 `embedseed=1` 时换成临时目录：
     * 不把用户缓存里的卡片拿去算向量；其它截图仍然用用户那份（首页要画"上次看到的卡片"）。
     */
    feedSnapshotDir: Path? = null,
    /**
     * 本地向量：不传就没有（演示 / 调试 / 截图默认如此）。
     * 真 App 在 `main` 里传 [feedhub.app.embed.DesktopOnnxTextEmbedder]：ORT CPU + q4 图，和安卓 CPU 路径
     * **同一个模型标识**；模型文件不在（没打包 / 没装）时它安静地不可用，设置页写「未包含模型」。
     *
     * 为什么不做成默认：截图 / 探针为了画一张图去加载约 155 MB 的会话（外加 224 MB 词表）不值得（要画"灌了向量之后"的样子时
     * 显式传一份，见 Screenshot 的 `embedseed=1`）。
     */
    embedder: feedhub.app.embed.Embedder? = null,
): AppGraph {
    val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main)
    // 酷安评论的人机验证：详情区里的覆盖层（CEF 在用完就退的子进程里）/ 退回系统浏览器，结果都回到本机临时服务（见 DesktopCaptchaHandler）
    val captchaPrompt = CaptchaPromptState()
    val captcha = DesktopCaptchaHandler(captchaPrompt)
    val core = proxyCoreOverride ?: if (!withProxyCore) {
        null
    } else {
        ProxyCoreBridge.locate(
            candidates = listOfNotNull(
                System.getenv(ProxyCoreBridge.envPath())?.takeIf { it.isNotBlank() }?.let { Paths.get(it) },
                System.getProperty("compose.application.resources.dir")
                    ?.takeIf { it.isNotBlank() }?.let { Paths.get(it, "sing-box") },
                LocalFiles.home.resolve("singbox"),
            ),
        )?.let { binary ->
            ProxyCore(
                binary = binary,
                dataDir = proxyDataDir ?: LocalFiles.home.resolve("singbox"),
                stateFile = proxyStateFile ?: LocalFiles.home.resolve("proxy-core.json"),
                scope = scope,
                // 「系统代理」那一档取订阅时问 JVM 的选择器（**按订阅地址的主机名**，和平台对象
                // 那条 `ProxyTarget.of(ProxyMode.System, hosts)` 同一个口径；解析不出来 = fail-closed）
                systemProxy = { host -> ProxyTarget.of(ProxyMode.System, listOf(host)) },
                expectedSha256 = ProxyCoreBridge.expectedSha256For(binary),
            )
        }
    }
    // 平台：生成的注册表里能跑在桌面上的插件（描述的 targets）；跑不了的灰显、附上描述里的原因
    val installed = InstalledPlugins.catalog
    // 对外接口：**App 自己**在本机 unix socket 上开只读 HTTP，给 agent 用
    // 。先建好壳再交给 AppGraph（它要这一份
    // graph 去取 Source / 设置 / 资料库），用 lateinit 打破这个环。
    lateinit var graphRef: AppGraph
    val appApi = appApiOverride
        ?: feedhub.app.api.DesktopAppApi({ graphRef }, apiSocketPath ?: feedhub.app.api.DesktopAppApi.defaultApiSocketPath())
    return AppGraph(
        ctx, scope,
        imageCacheDir = cacheDir().resolve("images"),
        library = library,
        cookieImporter = ChromeCookieImporter(ctx.cookies),
        desktop = true,
        // 账号页的「登录互传」：桌面只能出码（一次性局域网小服务 + 二维码），扫码在手机 / 平板上
        loginSync = feedhub.app.sync.LoginSync(canOffer = true),
        // 跨设备同步：桌面用 JFileChooser 选文件；设备密钥放 ~/.config/feedhub（新建一个 600 权限文件，不动已有的）
        syncFiles = feedhub.app.sync.DesktopSyncFiles(),
        deviceKeys = deviceKeys,
        deviceName = desktopDeviceName(),
        // 桌面能显示配对二维码；没有摄像头，另一端用 debug 的"手动输入二维码内容"（桌面 ↔ 桌面也能配对）
        pairing = feedhub.app.sync.SyncPairingUi(canHost = true, canScan = false, manualQrInput = true),
        sources = installed.forTarget(PluginTarget.DESKTOP).factories,
        unavailable = installed.unavailableOn(PluginTarget.DESKTOP),
        systemUiScale = UiScale.system,
        proxyCore = core,
        captcha = captcha,
        captchaPrompt = captchaPrompt,
        translationEngineFactory = if (BuildFeatures.TRANSLATE && DesktopTranslateRuntime.isBundledOrConfigured()) {
            { settings -> createDesktopTranslationEngine(settings) }
        } else {
            null
        },
        translationCacheDir = if (BuildFeatures.TRANSLATE) cacheDir() else null,
        translationSourceLanguages = if (BuildFeatures.TRANSLATE) BergamotModelFiles.SUPPORTED_SOURCES else emptySet(),
        translationModelId = if (BuildFeatures.TRANSLATE) BergamotModelFiles.MODEL_ID else null,
        translationRuntimeDescribe = if (BuildFeatures.TRANSLATE) {
            { settings -> DesktopTranslateRuntime.describe(settings.value.translationModelPath) }
        } else {
            null
        },
        feedSnapshotDir = feedSnapshotDir ?: cacheDir(),
        // 本地向量：桌面也接上了同一套调度器 / 库 / 筛选，推理在 ORT CPU 上
        embedder = embedder,
        // 导出按钮照样能用（`~/.config/feedhub/export/`）
        behaviorExport = feedhub.app.export.DesktopBehaviorExport(),
        // 对外接口（给 agent 用的本机 unix socket；开关默认关，见 AppSettings.appApi）
        appApi = appApi,
    ).also {
        graphRef = it
        // 解码能力账本落盘（`DecodeLedger`）：这次实测出来的上限跟着 state.json 走，重启后第一次播 4K 就避开。
        // 指纹每次现算（选定 VA 设备的 PCI vendor:device + 驱动串 + GStreamer 版本），换卡 / 升级驱动后旧记录作废。
        DecodeLedger.attach(ctx.store) { decodeEnvironmentFingerprint() }
    }
}

/**
 * 打开用户真实的资料库；打不开（文件坏了、被更新版本的程序改过、`.so` 加载失败）就退回内存里的，
 * 不让整个 App 起不来。
 *
 * `SqliteLibrary` 移入测试源集仅作对拍参照。
 */
fun openLibrary(): Library = try {
    RustLibrary.open(LocalFiles.home.resolve("library.db"))
} catch (e: Throwable) {
    System.err.println("本地资料库打不开，这次运行的收藏 / 历史只留在内存里：${e.message}")
    MemoryLibrary()
}

/** 配对时给对面看的名字：只用环境变量 / 用户名，**不做 DNS / 网络查询**。 */
private fun desktopDeviceName(): String {
    val host = System.getenv("HOSTNAME")?.trim().orEmpty()
    if (host.isNotEmpty()) return host.take(40)
    val user = System.getProperty("user.name")?.trim().orEmpty()
    return (if (user.isNotEmpty()) "$user 的电脑" else "桌面").take(40)
}

/**
 * 缓存的根目录（`$XDG_CACHE_HOME/feedhub`，没有就是 `~/.cache/feedhub`）：图片缓存、首页快照、
 * 翻译缓存都在下面。[internal] 是为了截图工具（`feedhub.app.dev`）在"演示本地向量"时能换成一个临时目录，
 * 不读用户缓存里的卡片、也不往用户的图片缓存里塞东西。
 */
internal fun cacheDir(): Path =
    System.getenv("XDG_CACHE_HOME")?.takeIf { it.isNotBlank() }?.let { Paths.get(it, "feedhub") }
        ?: Paths.get(System.getProperty("user.home"), ".cache", "feedhub")

/** 调试入口：`FEEDHUB_START=<platform>:<id>`，例如 `coolapk:74360444`；只用于本地验收，正常启动不设。 */
private fun debugStartItem(): FeedItem? {
    val raw = System.getenv("FEEDHUB_START")?.trim().orEmpty()
    val sep = raw.indexOf(':')
    if (sep <= 0) return null
    val platform = InstalledPlugins.catalog.find(raw.substring(0, sep)) ?: return null
    val id = raw.substring(sep + 1).trim()
    if (id.isEmpty()) return null
    return FeedItem(
        platform = platform,
        id = id,
        kind = ItemKind.POST,
        title = "调试打开 $id",
        detail = DetailTarget.Native(id),
    )
}

fun main(args: Array<String>) {
    // 验证码子进程（运行时里没有 bin/java 的老打包里，是 jpackage 启动器加这个参数起的，见 CaptchaHelperLauncher）：直接进它的入口，App 的东西一概不碰。
    // 放在最前面，CEF 只会在那个子进程里加载
    if (args.firstOrNull() == CaptchaHelperLauncher.FLAG) {
        feedhub.app.captcha.helper.CaptchaHelperMain.main(args.copyOfRange(1, args.size))
        return
    }
    // 字体预热最先起（后台线程，和下面的 AWT / Skia 初始化并行），见 FontWarmup
    val fonts = FontWarmup.start()
    // 缩放要在碰 AWT 之前设（建 AppGraph 会往 Swing 线程派任务，那就晚了），所以先单独读一次设置
    val ctx = LocalFiles.context()
    val wanted = UiScale.wanted(SettingsStore(ctx.store).value)
    val javaScale = UiScale.applyToJava(wanted)
    // 本地向量：开启 -Pfeedhub.embed=true 时桌面跑 ORT CPU + q4 图；基础版不创建
    val graph = desktopGraph(
        ctx = ctx,
        embedder = if (BuildFeatures.EMBED) {
            feedhub.app.embed.DesktopOnnxTextEmbedder(log = { System.err.println(it) })
        } else {
            null
        },
    )
    // 调试入口：FEEDHUB_START=coolapk:74360444 直接打开一条原生详情（截图 / 验收验证码用，正常启动不设就没有）
    val startItem = debugStartItem()
    // 小数部分（例如 150% 时 Java 只给了 1 倍）让窗口初始大小也跟上
    val sizeFactor = wanted / javaScale
    // 关窗口时要先把窗口藏起来，但 `onCloseRequest` 拿不到 FrameWindowScope 里的 window，所以在内容里记一份
    val windowRef = java.util.concurrent.atomic.AtomicReference<java.awt.Window?>()
    application {
        val windowState = rememberWindowState(width = (1280 * sizeFactor).dp, height = (820 * sizeFactor).dp)
        Window(
            onCloseRequest = {
                // 1. 先藏窗口：KWin 会在销毁前把最大化的窗口还原成普通大小，看起来像「先缩小再关闭」；
                //    设成不可见之后窗口管理器就没有可还原的东西了。
                runCatching { windowRef.get()?.isVisible = false }
                // 2. 同步收尾（最多 3 秒）：播放进度落盘、设置里还在防抖的那次立即落盘、翻译缓存 / llama-server 收尾。
                //    不用 exitApplication：它先 dispose AWT 窗口，又回到「先缩小再关闭」。
                graph.shutdown()
                // 3. 直接结束进程：窗口已经隐藏，随 X 连接一起销毁。
                exitProcess(0)
            },
            title = "feedhub",
            state = windowState,
        ) {
            SideEffect { windowRef.set(window) }
            // Compose 的密度 = Java 给的倍数；和想要的不一样（小数缩放、或设置里刚改过）就在这里改，改设置立刻生效
            val base = LocalDensity.current
            val want = UiScale.wanted(graph.settings.value)
            val density = if (abs(want - base.density) > 0.01f) Density(want, base.fontScale) else base
            // 最小化 = 看不见：视频不画帧、进度条 / 弹幕不跑（LocalWindowVisible）
            // 插电 / 电池：预加载多激进、图片内存缓存多大（LocalPluggedIn，见 PreloadPolicy）
            CompositionLocalProvider(
                LocalDensity provides density,
                LocalWindowVisible provides !windowState.isMinimized,
                LocalPluggedIn provides rememberPluggedIn(),
            ) {
                fonts.Provide { App(graph, initialDetail = startItem) }
            }
        }
    }
}
