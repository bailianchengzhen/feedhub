package feedhub.app

import android.app.Application

/**
 * 安卓进程入口：
 *
 * 1. **进程级单例 [graph]**：把 [AppGraph] 从 `MainActivity` 提升到 `Application` 层，和 `Coil`、
 *    `AppGraph.current`、`ActivePlaybackSessions`、`AndroidPlayback` 等进程级设施生命周期一致；
 *    返回键退出再秒开不会新建第二套 `RustLibrary` / `AppGraph`，也不会出现旧 Activity 收尾线程误杀新播放器与
 *    新 `sing-box` 的竞争（B2）。需要 `Activity` 的部分（SAF、刷新率、帧指标、同栈跳转）通过 [uiBindings]
 *    按 `MainActivity` 生命周期绑定与解绑，零泄漏。
 * 2. **后台预热 `UniffiLib`**：进程一创建就在后台线程加载 `libfeedhub_ffi.so` 并触发 JNA 符号注册与校验和，
 *    与 `Activity` 构造并行执行，减轻主线程首帧耗时。
 */
class FeedhubApplication : Application() {
    internal val uiBindings: AndroidUiBindings by lazy(LazyThreadSafetyMode.SYNCHRONIZED) {
        AndroidUiBindings(this)
    }

    /**
     * 真实数据的那一份（release 的常态）。演示数据（debug / 演示包 + `--ez demo true`）不走这里：
     * `MainActivity` 自己建一份、自己关（见 [MainActivity.onCreate]）。
     */
    val graph: AppGraph by lazy(LazyThreadSafetyMode.SYNCHRONIZED) {
        androidGraph(
            context = this,
            demo = false,
            library = openAndroidLibrary(this),
            only = null,
            captcha = null,
            demoFeedMode = feedhub.app.demo.DemoFeedMode.NORMAL,
            demoFeedDelayMs = 0,
            translateDemo = false,
            uiBindings = uiBindings,
        )
    }

    override fun onCreate() {
        super.onCreate()
        Thread(
            {
                runCatching {
                    // RustPerf.readProcSelfStat 内部先调 RustNative.ensureLoaded()，再触发 UniffiLib.<clinit>
                    feedhub.perf.RustPerf.readProcSelfStat()
                }
            },
            "feedhub-ffi-prewarm",
        ).apply { isDaemon = true }.start()
    }
}
