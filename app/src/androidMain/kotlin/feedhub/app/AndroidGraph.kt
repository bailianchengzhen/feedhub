package feedhub.app

import feedhub.app.sync.AndroidSyncFiles
import feedhub.app.sync.LoginSync
import feedhub.core.plugin.PlatformCatalog
import feedhub.core.plugin.PluginTarget
import feedhub.plugins.InstalledPlugins
import android.app.Activity
import android.content.Context
import feedhub.app.captcha.AndroidCaptchaHandler
import feedhub.app.demo.androidDemoGraph
import feedhub.app.display.NoFrameRateSink
import feedhub.app.display.WindowFrameRateSink
import feedhub.app.library.Library
import feedhub.app.library.MemoryLibrary
import feedhub.app.library.RustLibrary
import feedhub.app.login.AndroidWebLogin
import feedhub.app.translate.AndroidTranslationEngine
import feedhub.app.translate.BergamotModelFiles
import feedhub.app.video.AndroidPlayback
import feedhub.core.KeyValueStore
import feedhub.cookies.RustCookieJar
import feedhub.core.Platform
import feedhub.core.SourceContext
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import okhttp3.OkHttpClient
import java.io.File
import java.nio.file.Paths

/**
 * 进程级 `AppGraph` 需要按 Activity 生命周期动态挂接/解绑的四个窗口级组件（A4）：
 * - [syncFiles]：`ActivityResultRegistry` 文档选择器；
 * - [frameRateSink]：窗口最高刷新率请求；
 * - [systemActions]：同任务栈启动 Custom Tabs / 分享面板；
 * - [perfSampler]：`Window.OnFrameMetricsAvailableListener` 帧耗时统计。
 *
 * Activity 重建或返回键退出时解绑旧实例，绝不通过进程级 `AppGraph` 泄漏已销毁的 Activity。
 */
internal class AndroidUiBindings(context: Context) {
    val syncFiles = AndroidSyncFiles(context as? androidx.activity.ComponentActivity)
    val frameRateSink = WindowFrameRateSink(context as? Activity)
    val systemActions = AndroidSystemActions(context)
    val perfSampler = feedhub.app.perf.AndroidPerfSampler(
        context.applicationContext ?: context,
        (context as? Activity)?.window,
    )

    fun attach(activity: androidx.activity.ComponentActivity) {
        syncFiles.attach(activity)
        frameRateSink.attach(activity)
        systemActions.attach(activity)
        perfSampler.setWindow(activity.window)
    }

    fun detach(activity: androidx.activity.ComponentActivity) {
        frameRateSink.detach(activity)
        systemActions.detach(activity)
        perfSampler.clearWindow(activity.window)
        syncFiles.detach(activity)
    }
}

/**
 * 安卓版的组装（和 `desktopMain` 的 `desktopGraph` 对应）。桌面的 Cookie / 设置在 `~/.config/feedhub` 下的
 * JSON 文件里（`feedhub.core.local.LocalFiles`，走 java.nio 和 POSIX 权限），安卓这边：
 *
 * | 东西 | 桌面 | 安卓 |
 * |---|---|---|
 * | Cookie | `cookies.json`（FileCookieJar） | `filesDir/cookies.json`（[RustCookieJar]，落盘 / 锁都在 Rust，权限靠应用沙箱） |
 * | 设置 / 键值存储 | `state.json`（FileKeyValueStore） | [SharedPreferencesStore]（`feedhub` 这个 xml） |
 * | 收藏 / 历史 / 屏蔽 | `~/.config/feedhub/library.db` | `filesDir/library/library.db`（同一个 `RustLibrary`） |
 * | 图片磁盘缓存 | `~/.cache/feedhub/images` | `cacheDir/images` |
 * | 图片 / 接口客户端 | 本机回环上可能还有代理 | 同样是 OkHttp，代理设置跟着 [SourceContext] 走 |
 *
 * **默认 `demo = true`**：只用演示数据、一个请求也不发（截图 / 离线测试用）。真实数据（`demo = false`）是
 * release 的默认，见 `MainActivity`：接口、图片、播放都走真实的 Source，登录用系统 WebView（[AndroidWebLogin]）。
 *
 * `systemActions = AndroidSystemActions(context)`：安卓有系统界面可调（系统分享面板、Custom Tabs 打开外链、
 * 官方 App 深链）；桌面那份是 [SystemActions.None]，分享 / 外链照旧复制。
 *
 * 安卓没有）"——那个判断是错的，quickjs-kt 是 KMP 库，
 * 安卓变体（`quickjs-kt-android`，AAR 里带 `arm64-v8a` / `x86_64` 的 `libquickjs.so`）一直都在，
 * 模拟器上解出来的 n/sig 和 JVM 逐字一致。现在所有插件都是纯
 * JVM / OkHttp（+ quickjs），安卓直接用；[AppGraph.unavailable] 在安卓上是空的。
 *
 * @param demo true = 用 [androidDemoGraph] 的演示源；false = 用真实的 Source（生成的注册表 `InstalledPlugins`）。
 * @param only 只开放这几个平台（null = 全部）。**debug 启动参数用**（`DebugStartActivity` 的
 *   `--es platforms bili,x`）：截图 / 离线验证时把请求压到一两个平台，不碰别的平台。
 * @param library 本地资料库；默认 `filesDir/library/library.db`。
 * @param captcha 人机验证（酷安评论的网易易盾）：默认 [AndroidCaptchaHandler]；debug 的假验证页入口可以换一份
 *   （见 `DebugStartActivity` 的 `--es captcha fake`）。
 */
fun androidGraph(
    context: Context,
    demo: Boolean = true,
    library: Library = openAndroidLibrary(context.applicationContext ?: context),
    only: Set<Platform>? = null,
    captcha: AndroidCaptchaHandler? = null,
    /** 只给空状态截图：演示源演成"没内容 / 没登录"（`--es emptyfeed`，见 [DemoFeedMode]）。 */
    demoFeedMode: feedhub.app.demo.DemoFeedMode = feedhub.app.demo.DemoFeedMode.NORMAL,
    /**
     * 只给首帧截图：演示源取信息流时先等这么多毫秒
     * （`--es netdelay 3000`），第一次刷新拖长才截得到"内容还没上屏"的那一帧。**不发任何请求**。
     */
    demoFeedDelayMs: Long = 0,
    /**
     * 「X 式」翻译界面的演示：演示组装接上假引擎并把总开关打开。
     * **只在演示数据下有作用**。
     */
    translateDemo: Boolean = false,
): AppGraph = androidGraph(
    context, demo, library, only, captcha, demoFeedMode, demoFeedDelayMs, translateDemo,
    uiBindings = null,
)

/** [androidGraph] 的完整版：进程级组装（[FeedhubApplication]）传 [uiBindings]，让窗口级组件跟着 Activity 挂接 / 解绑。 */
internal fun androidGraph(
    context: Context,
    demo: Boolean,
    library: Library,
    only: Set<Platform>?,
    captcha: AndroidCaptchaHandler?,
    demoFeedMode: feedhub.app.demo.DemoFeedMode,
    demoFeedDelayMs: Long,
    translateDemo: Boolean,
    uiBindings: AndroidUiBindings?,
): AppGraph {
    val appContext = context.applicationContext ?: context
    // Media3 的 ExoPlayer / PlayerView 要有 Context，而 createPlayerEngine(client) 只拿得到 OkHttp 客户端：
    // 在这里记一份应用上下文（见 AndroidPlayback）
    AndroidPlayback.remember(appContext)
    // 验证码 handler 一直是有的（真实数据遇到酷安评论的易盾时用）；演示 / 截图不触发它，给一份也无害
    val captchaHandler = captcha ?: AndroidCaptchaHandler()
    if (demo) {
        return androidDemoGraph(context, library, only, captchaHandler, demoFeedMode, demoFeedDelayMs, translateDemo)
    }

    // debug 构建才给「手动粘贴二维码内容」（模拟器没摄像头 / 真机吞触摸时代替扫码）
    val debuggable = (appContext.applicationInfo.flags and android.content.pm.ApplicationInfo.FLAG_DEBUGGABLE) != 0
    // Cookie 罐：罐子 + 落盘（原子替换 / 坏文件留底 /
    // `cookies.json.lock` 跨进程锁）都在 Rust，这里只有"文件放哪"。安卓只有一个进程，
    // 那把 `fcntl` 锁在本进程里也只是排队（无害）。
    val jar = RustCookieJar.open(File(appContext.filesDir, "cookies.json"))
    val http = OkHttpClient.Builder().cookieJar(jar).build()
    val ctx = SourceContext(http, jar, SharedPreferencesStore(appContext))
    val wanted = androidPlatforms(only)
    val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main)
    // 性能记录：记录文件放外置的应用目录（`getExternalFilesDir("perf")`），
    // 采集器要窗口才能拿帧指标；应用上下文（预览 / 测试）没有窗口就只少帧统计这一项。
    val perfStore = feedhub.app.perf.PerfFileStore({ appContext.getExternalFilesDir(feedhub.app.perf.PerfFileStore.ANDROID_DIR_NAME) })
    // 内置代理核心（sing-box 子进程）：只有 nativeLibraryDir 里真有那份二进制时才组装
    // （没有就不显示「内置核心」那一档，免得选了个一定起不来的档位），见 androidProxyCore。
    val core = androidProxyCore(appContext, scope)
    // 本地向量 + 封面向量：默认基础版（!BuildFeatures.EMBED）整块不编进 APK，返回空包；
    // 带了 -Pfeedhub.embed=true 或 -Pfeedhub.embedNpu=true 时由 androidEmbed 源集组装 CPU / NPU 路由与封面图。
    val embedBundle = if (BuildFeatures.EMBED) {
        createAndroidEmbedBundle(appContext, scope, ::androidLogLine)
    } else {
        AndroidEmbedBundle()
    }
    // 窗口级组件：进程级组装从 uiBindings 拿（跟着 Activity 挂接 / 解绑），否则按传进来的 context 现建
    val syncFiles = uiBindings?.syncFiles
        ?: (context as? androidx.activity.ComponentActivity)?.let { AndroidSyncFiles(it) }
    val frameRateSink = uiBindings?.frameRateSink
        ?: (context as? Activity)?.let { WindowFrameRateSink(it) }
        ?: NoFrameRateSink
    val systemActions = uiBindings?.systemActions ?: AndroidSystemActions(context)
    val perfSampler = uiBindings?.perfSampler
        ?: feedhub.app.perf.AndroidPerfSampler(appContext, (context as? Activity)?.window)
    return AppGraph(
        ctx, scope,
        imageCacheDir = imageCacheDir(appContext),
        library = library,
        systemActions = systemActions,
        desktop = false,
        sources = InstalledPlugins.catalog.factories.filterKeys { it in wanted },
        unavailable = androidUnavailable(only),
        webLogin = AndroidWebLogin(),
        loginSync = LoginSync(canOffer = true, canScan = true, manualQrInput = debuggable),
        // 跨设备同步：安卓用系统文档选择器（SAF）；进程级组装由 uiBindings 按 Activity 挂接，
        // 拿不到 Activity 的组装（预览 / 测试）就没有
        syncFiles = syncFiles,
        // 设备密钥放 App 私有目录（一个 600 权限的小文件，只碰自己这个文件名）
        deviceKeys = feedhub.core.sync.DeviceKeyStore(java.io.File(appContext.filesDir, "sync").toPath()),
        deviceName = (android.os.Build.MODEL ?: "安卓设备").take(40),
        // 手机能扫码（CameraX），也能反过来显示二维码让另一台手机 / 桌面扫；debug 构建另有手动粘贴
        pairing = feedhub.app.sync.SyncPairingUi(canHost = true, canScan = true, manualQrInput = debuggable),
        proxyCore = core,
        // 安卓：核心只在前台跑（前台 + 退到后台 30 秒宽限），前后台事件由 MainActivity 的 onStart / onStop 喂
        coreForeground = core != null,
        showBuiltinProxy = core != null,
        // 出口锁关着而直连时那一行日志：桌面走 stderr，安卓只有 logcat 看得见
        logLine = { line -> android.util.Log.i(PROXY_LOG_TAG, line) },
        // 帧率请求：滑动 / 视频在播时向系统要高刷、静止 / 后台撤回。
        // 只有拿到 Activity 才能碰 Window / Surface；应用上下文（预览、测试）就退回空实现。
        frameRateSink = frameRateSink,
        // 酷安评论要人机验证时的覆盖层：handler 写 prompt，详情页里的 CaptchaOverlay 读
        captcha = captchaHandler,
        captchaPrompt = captchaHandler.prompt,
        // 本地向量：基础版为 null；开 -Pfeedhub.embed=true / embedNpu=true 时由 androidEmbed 组装。
        embedder = embedBundle.embedder,
        // 封面向量：基础版为 null；NPU 包里为两张 HTP 图 + Coil 封面图来源。
        coverEmbedder = embedBundle.coverEmbedder,
        coverImages = embedBundle.coverImages,
        // 行为数据导出：写到 getExternalFilesDir(null)/export/，release 包也能用
        behaviorExport = feedhub.app.export.AndroidBehaviorExport(appContext),
        // 本地翻译（Project Bergamot 进程内轻量英译中引擎）：默认基础版（!BuildFeatures.TRANSLATE）
        // 不带翻译模型和运行库，直接给 null；开启 -Pfeedhub.translate=true 且包内资源齐全时懒建引擎。
        translationEngineFactory = if (BuildFeatures.TRANSLATE && AndroidTranslationEngine.isBundled(appContext)) {
            { settings -> AndroidTranslationEngine(appContext, settings, translateCacheDir(appContext)) }
        } else {
            null
        },
        // 翻译缓存（`translations.jsonl`）放 App 私有缓存目录（基础版不建）
        translationCacheDir = if (BuildFeatures.TRANSLATE) translateCacheDir(appContext) else null,
        translationSourceLanguages = if (BuildFeatures.TRANSLATE) BergamotModelFiles.SUPPORTED_SOURCES else emptySet(),
        translationModelId = if (BuildFeatures.TRANSLATE) BergamotModelFiles.MODEL_ID else null,
        translationRuntimeDescribe = if (BuildFeatures.TRANSLATE) {
            { _ -> AndroidTranslationEngine.describe(appContext) }
        } else {
            null
        },
        // 性能记录器：开关在设置里（默认关）。导出口子把文件复制到系统下载目录，给 adb pull 兜底，
        // 见 AndroidPerfExport 的 KDoc（Android 11+ 非 debuggable 包可能拉不到 Android/data）。
        perf = feedhub.app.perf.PerfRecorder(
            sampler = perfSampler,
            store = perfStore,
            scope = scope,
            threadCpuMs = { android.os.SystemClock.currentThreadTimeMillis() },
            exporter = { feedhub.app.perf.AndroidPerfExport.export(appContext, perfStore.files()) },
        ),
    )
}

/**
 * 一份**独立**的取图（不走 `AppGraph.current`）：只在 `AppGraph` 还没建好就被要图时兜底
 * （封面向量的构造期）。判定和 `AppGraph.imageProxy` 一样：认不出域名就跟随系统代理，
 * 解析不出来是 fail-closed。
 */
internal fun androidImageFetch(): feedhub.app.image.ImageFetch =
    feedhub.app.image.DefaultHeadersImageFetch(
        delegate = feedhub.app.image.RustImageFetch(
            proxyFor = { host -> feedhub.rust.ProxyTarget.of(feedhub.core.ProxyMode.System, listOf(host)) },
        ),
        headers = mapOf("User-Agent" to feedhub.core.UserAgents.DESKTOP_CHROME),
    )

/** 代理相关日志的 logcat tag（安卓上 stderr 看不到，只有 logcat）。 */
internal const val PROXY_LOG_TAG = "feedhub-proxy"

/**
 * 安卓上可用的平台：装了的插件里**描述的 targets 有安卓**的，
 * 再按 [only] 过滤。
 */
internal fun androidPlatforms(only: Set<Platform>? = null, platforms: PlatformCatalog = InstalledPlugins.catalog): Set<Platform> =
    platforms.forTarget(PluginTarget.ANDROID).platforms.filterTo(LinkedHashSet()) { only == null || it in only }

/**
 * 安卓上"暂时用不了"的平台 → 一行原因（插件描述的 unavailableNote；设置页显示、Source 不构造）。
 * 这里返回空表；机制留着，
 * 以后有只做桌面的插件时照样生效（`app/src/desktopTest` 的 `AppGraphAvailabilityTest` 守着）。
 */
internal fun androidUnavailable(only: Set<Platform>? = null, platforms: PlatformCatalog = InstalledPlugins.catalog): Map<Platform, String> =
    platforms.unavailableOn(PluginTarget.ANDROID).filterKeys { only == null || it in only }

/** 图片磁盘缓存目录：`<app cache>/images`。 */
internal fun imageCacheDir(context: Context): java.nio.file.Path = Paths.get(context.cacheDir.path, "images")

/**
 * 翻译的落盘目录：`<app cache>/translate`（`translations.jsonl` 和 `llama-server.log` 都在下面）。
 *
 * 和桌面 `~/.cache/feedhub` 一个定位：**缓存**，系统可以清；清了只是重翻一遍。
 * 模型**不放这儿**（1.1 GB 的模型放 `noBackupFilesDir`，见 `AndroidTranslationEngine.ensureModelFile`：
 * 缓存目录会被系统在空间不足时清掉，那意味着下次用要重新从 assets 拷 1.1 GB）。
 */
internal fun translateCacheDir(context: Context): java.nio.file.Path =
    Paths.get(context.cacheDir.path, "translate")

/**
 * 打开 `filesDir/library/library.db`；打不开（文件坏了、被更新版本的程序改过、`.so` 加载失败）就退回内存里的，
 * 不让整个 App 起不来——和桌面版 `Main.kt` 的 `openLibrary` 一个思路。
 *
 */
fun openAndroidLibrary(context: Context): Library = try {
    val dir = File(context.filesDir, "library")
    dir.mkdirs()
    RustLibrary.open(File(dir, "library.db").toPath())
} catch (e: Throwable) {
    System.err.println("本地资料库打不开，这次运行的收藏 / 历史只留在内存里：${e.message}")
    MemoryLibrary()
}

/**
 * 用 `SharedPreferences` 实现的 [KeyValueStore]（各平台存设备标识 / 签名密钥缓存用）。
 * 接口的 KDoc 里写的就是"App 里用 SharedPreferences 实现"。
 */
class SharedPreferencesStore(context: Context, name: String = "feedhub") : KeyValueStore {
    private val prefs = context.applicationContext.getSharedPreferences(name, Context.MODE_PRIVATE)

    override fun get(key: String): String? = prefs.getString(key, null)

    override fun put(key: String, value: String?) {
        prefs.edit().apply { if (value == null) remove(key) else putString(key, value) }.apply()
    }
}

/** 向量 / 导出这些内部日志也走 logcat（和代理那行同一个 tag 前缀，方便 `adb logcat -s feedhub-embed`）。 */
private fun androidLogLine(line: String) {
    android.util.Log.i("feedhub-embed", line)
}

/**
 * 安卓本地向量的可选组装包：基础版（`!BuildFeatures.EMBED`，源集 `androidNoEmbed`）返回全 null，
 * 连 `onnxruntime-android` AAR 都不进编译类路径和 APK；开 `-Pfeedhub.embed=true`（或 `-Pfeedhub.embedNpu=true`）
 * 时由 `androidEmbed` 源集里的 `createAndroidEmbedBundle` 提供真实实现。
 */
internal data class AndroidEmbedBundle(
    val embedder: feedhub.app.embed.Embedder? = null,
    val coverEmbedder: feedhub.app.embed.CoverEmbedder? = null,
    val coverImages: feedhub.app.embed.CoverImageSource? = null,
)
