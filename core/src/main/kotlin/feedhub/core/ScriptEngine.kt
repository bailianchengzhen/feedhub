package feedhub.core

/**
 * 可选能力：这个平台有一个**要在设备上跑外来 JS 的签名引擎**（现在只有 YouTube：yt-dlp 的 EJS 跑在
 * quickjs-kt 里解 n/sig），给它一个"这台设备上到底跑不跑得起来"的自检口子。
 *
 * 为什么放在 core、而不是让测试直接调插件：`:androidApp` 的 instrumented 测试
 * （`YoutubeEjsAndroidTest`）要在模拟器 / 真机上真的跑一遍签名引擎，而它**不能 import 任何插件的类**
 * ——`PluginBoundaryTest` 守着"从 plugins.toml 删掉一个插件，别的模块照样编得过"。所以测试只能从注册表
 * （`InstalledPlugins.catalog` → [feedhub.core.plugin.PlatformPlugin.create]）拿到一个 [Source]，
 * 再按这个核心接口问一句。生产路径不调它（`YoutubeSource.play` 还是走自己的内部实现）。
 *
 * 实现方：`scriptEngineReady` 说脚本读得到（安卓上要求它们真的打进 APK 的 java 资源）；
 * `solveChallenges` 解一组挑战，**每次调用自己建运行时、跑完就关**（和 App 里"整个 Source 共用一个、
 * 空闲 20 秒释放"不是一回事，自检要看第一次初始化的耗时 / 内存）。
 */
interface ScriptEngineProbe : Source {
    /**
     * 引擎自带的那几段脚本能不能从 class 路径读到。
     * 桌面 / 安卓都该是 true；false 说明打包方式变了（安卓的 java 资源没进 APK）。
     */
    fun scriptEngineReady(): Boolean

    /**
     * 解一组挑战：[requests] 是 `类型 → 挑战列表`（YouTube 的类型是 `"sig"` / `"n"`），
     * 返回"挑战 → 答案"。纯本地计算，**不发任何请求**。
     */
    suspend fun solveChallenges(
        playerJs: String,
        playerKey: String,
        requests: List<Pair<String, List<String>>>,
    ): Map<String, String>
}
