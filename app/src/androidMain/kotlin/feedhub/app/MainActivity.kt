package feedhub.app

import android.content.Context
import android.content.Intent
import android.content.pm.ApplicationInfo
import android.content.pm.PackageManager
import android.graphics.drawable.ColorDrawable
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.ui.platform.LocalConfiguration
import feedhub.app.ui.DarkMode
import feedhub.app.video.AppOrientation
import feedhub.app.video.LocalWindowVisible
import kotlinx.coroutines.cancel

/**
 * 安卓的入口（声明在 :androidApp 的 AndroidManifest 里）。整个界面就是 commonMain 的 [App]，
 * 和桌面版共用一份代码；[AppGraph] 由 [FeedhubApplication] 按进程单例持有，
 * Activity 只负责把窗口级能力挂上 / 解绑，并向界面提供真实的 [LocalWindowVisible]（B5）。
 *
 * 用哪份数据：
 * - **release：真实数据**（`androidGraph(this, demo = false)`）：接口、图片、播放都是真的，登录态存在
 *   App 私有目录（`filesDir/cookies.json` + SharedPreferences），登录入口是系统 WebView；
 * - **debug：`--ez demo true` 才用演示数据**（一个请求也不发，见 [isDemoRequested]），默认也是真实数据。
 *   截图 / 自检要用演示数据时加这个参数：
 *   `adb -s emulator-5554 shell am start -n feedhub.app/.MainActivity --ez demo true`
 *
 * 系统栏：`enableEdgeToEdge()`，界面自己用 Scaffold 的 insets 躲开状态栏 / 手势条（Compose 侧处理）。
 *
 * debug 构建里还有一个**只在 debug 存在**的入口 `feedhub.app.android.DebugStartActivity`（`:androidApp` 的
 * `src/debug`）：按 `am start` 的 `--es screen … --es item …` 直接开某一页，真机上不用点按（真机吞掉 adb
 * 注入的触摸），用法见 [feedhub.app.startScreenOf]。release 里没有它。
 */
class MainActivity : ComponentActivity() {
    private var graph: AppGraph? = null

    /** 这个 Activity 自己建的图（演示数据 / 没有 [FeedhubApplication] 的环境）：销毁时自己收尾。 */
    private var ownsGraph = false
    private var windowVisible by mutableStateOf(false)

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        enableEdgeToEdge()
        val app = application as? FeedhubApplication
        val demo = isDemoRequested(this, intent)
        val g = if (app != null && !demo) {
            // 真实数据：进程级那一份，窗口级组件跟着这个 Activity 挂接（onDestroy 解绑）
            app.uiBindings.attach(this)
            ownsGraph = false
            app.graph
        } else {
            ownsGraph = true
            androidGraph(this, demo = demo)
        }
        graph = g
        // 演示 / 自检：`--es embedprobe "中文|English"` 把这两条文本的向量写到外置目录（给 Kotlin vs Python 对拍用）。
        // 和 demo 同一个门槛（release 不认），见 feedhub.app.demo.runEmbedProbeIfRequested。
        if (demo) feedhub.app.demo.runEmbedProbeIfRequested(this, intent)
        applyStartWindowBackground(g.settings.value.darkMode)
        setContent {
            // 屏幕方向：手机默认锁竖屏，设置里「自动旋转」开了才跟系统（见 AppOrientation）；折叠屏展开 / 合上时重算
            val autoRotate = g.settings.value.autoRotate
            val smallestWidthDp = LocalConfiguration.current.smallestScreenWidthDp
            LaunchedEffect(autoRotate, smallestWidthDp) {
                AppOrientation.setBase(this@MainActivity, autoRotate, smallestWidthDp)
            }
            CompositionLocalProvider(LocalWindowVisible provides windowVisible) {
                App(g)
            }
        }
    }

    // 内置代理核心只在前台跑：进前台 / 退到后台喂给核心（前台 + 退后台 30 秒宽限期，判定表在 Rust：
    // `proxy_core::foreground::ForegroundGate`）。没有核心的组装这里是 null，什么也不做。
    // 另外把前后台喂给 [AppGraph.onForeground]：后台停掉刷新 / 预取、撤回帧率请求，
    // 回来按"离上次刷新多久"只预取、不自动替换屏幕上的内容。
    override fun onStart() {
        super.onStart()
        windowVisible = true
        graph?.applyForeground(true)
        graph?.onForeground(true)
    }

    // 回前台：切走前在播的视频接着播（用户自己暂停的不动）
    override fun onResume() {
        super.onResume()
        graph?.resumePlayback()
    }

    // 锁屏 / 切走时声音要立刻断：onPause 比 onStop 早（不等锁屏动画走完）
    override fun onPause() {
        super.onPause()
        graph?.pausePlayback()
    }

    override fun onStop() {
        super.onStop()
        windowVisible = false
        graph?.applyForeground(false)
        graph?.onForeground(false)
    }

    // 进程级 AppGraph 跟 FeedhubApplication 共生死；
    // 这里只解绑当前 Activity 的窗口引用，避免泄漏。非 FeedhubApplication 的兜底环境（如独立测试）仍照常收尾。
    override fun onDestroy() {
        super.onDestroy()
        (application as? FeedhubApplication)?.uiBindings?.detach(this)
        val g = graph ?: return
        graph = null
        // 自己建的图换配置（旋转）也要收尾：新的 Activity 会再建一份，不收尾旧的就漏了
        if (!ownsGraph) return
        ownsGraph = false
        Thread({ g.shutdown() }, "feedhub-activity-destroy").start()
        g.scope.cancel()
    }
}

/**
 * 冷启动那一帧的窗口底色跟着 **App 自己的**深色设置。
 *
 * 主题里的 `windowBackground` 分 day / night 两档（`values/themes.xml` 和 `values-night/themes.xml`），但那只跟**系统**夜间模式走；
 * App 的深色模式是设置里选的，"系统浅色 + App 深色"时冷启动会先白一下。这里在第一次绘制之前按设置改一次窗口底色
 * （`DarkMode.SYSTEM` 不动，主题那两档已经是对的）。注意 Android 12+ 的系统启动画面（SplashScreen）用的是主题里的
 * `windowSplashScreenBackground`，改不到——那一档仍然是 day / night，见 `values-v31/themes.xml`。
 */
private fun MainActivity.applyStartWindowBackground(mode: DarkMode) {
    val color = when (mode) {
        DarkMode.DARK -> 0xFF121417.toInt() // = Theme.kt 的 Dark.background
        DarkMode.LIGHT -> 0xFFF6F7F9.toInt() // = Theme.kt 的 Light.background
        DarkMode.SYSTEM -> return
    }
    window.setBackgroundDrawable(ColorDrawable(color))
}

/**
 * 这次启动要不要用演示数据：**只有可调试的构建，或者带 `feedhub.demoBuild = true` 那条 meta-data 的构建**
 * （`benchmark`，见 `androidApp/src/main/AndroidManifest.xml`）才认 `--ez demo true`。
 * release 里这个参数一律无效，免得"随手加个参数"就把真实数据关掉。演示数据一个请求也不发，见 `androidDemoGraph`。
 */
fun isDemoRequested(context: Context, intent: Intent?): Boolean {
    val debuggable = (context.applicationInfo.flags and ApplicationInfo.FLAG_DEBUGGABLE) != 0
    // 两个坑：
    // 1. `context.applicationInfo.metaData` 在 ActivityThread 给的那份 ApplicationInfo 里是 **null**
    //    （要带 GET_META_DATA 标记重新问一遍 PackageManager 才有），必须走 getApplicationInfo；
    // 2. manifest 里 `<meta-data android:value="true">` 编译成的是 Boolean（不是字符串），
    //    `Bundle.getBoolean` / `getString` 单独用会在另一种类型上静默返回默认值，这里按 Object 读、两种都认。
    val rawMeta = runCatching {
        context.packageManager.getApplicationInfo(context.packageName, PackageManager.GET_META_DATA)
            .metaData?.get("feedhub.demoBuild")
    }.getOrNull()
    val demoBuild = when (val raw = rawMeta) {
        is Boolean -> raw
        is String -> raw.equals("true", ignoreCase = true)
        else -> false
    }
    return demoDataRequested(
        debuggable = debuggable,
        demoBuild = demoBuild,
        demoExtra = intent?.getBooleanExtra("demo", false) == true,
    )
}

/**
 * [isDemoRequested] 的纯判定（没有 Android 类型，`androidApp` 的离线单元测试直接测它）：
 * 演示数据 = 这次启动带了 `demo=true`，**并且**是允许演示数据的构建（可调试，或 benchmark 的 meta-data）。
 * 三个参数都是 false 时（= release 的常规启动）永远是 false。
 */
fun demoDataRequested(debuggable: Boolean, demoBuild: Boolean, demoExtra: Boolean): Boolean =
    demoExtra && (debuggable || demoBuild)
