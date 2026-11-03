package feedhub.app

import feedhub.app.settings.AdvancedSection
import feedhub.core.Platform
import feedhub.core.plugin.PlatformCatalog
import feedhub.plugins.InstalledPlugins

/**
 * 起始页：debug 构建的启动参数（`screen` / `item`）解析出来的结果。
 *
 * 为什么要这个：部分安卓真机（Android 16）把 adb 注入的触摸当"不受信任"吞掉，所以真机截图不能靠点按到达各个页面。改成"启动时就告诉 App 开哪一页"，
 * 一个点按都不需要。参数名和桌面截图工具的 `dev/Screenshot.kt` 对齐。
 *
 * 只有 debug 构建认这组参数：解析在这个文件里（commonMain，便于离线测试），但真正读 Intent 的入口
 * `DebugStartActivity` 只存在于 `:androidApp` 的 `src/debug` 源集，release 构建里连这个 Activity 都没有。
 *
 * 用法（真机 / 模拟器都一样，Activity 名是 debug 专属的）：
 * ```
 * adb -s <SERIAL> shell am start -n feedhub.app/feedhub.app.android.DebugStartActivity \
 *     --es screen detail --es item zhihu-1
 * ```
 *
 * - `screen`：`home`（默认）/ `following` / `mine` / `settings` / `advanced`/
 *   `advanced:<子页>`（直接开某个进阶子页，如 `advanced:proxy`、`advanced:behavior`，见
 *   [feedhub.app.settings.AdvancedSection]）/ `detail`（图文详情）/ `video`（视频详情）
 * - `item`：演示条目的 id `<平台key>-<序号>`（`bili-0`、`zhihu-1`…，序号 0..5，见 [feedhub.app.demo] 的生成规则）；
 *   只写序号（`item=2`）按第一个插件（`plugins.toml` 的第一行，现在是 B站）算。`detail` / `video` 不给 `item` 时用默认条目（[DEFAULT_DETAIL_ITEM] /
 *   [DEFAULT_VIDEO_ITEM]）。只给 `item`、不给 `screen` 等于 `screen=detail`。
 *
 * 认不出来**抛异常**，不静默退回首页：参数写错时截图会看不出是参数错了还是界面错了。
 * `DebugStartActivity` 接住它，退回首页并把原因打进 logcat（tag `feedhub-debug`）。
 */
data class StartScreen(
    val tab: Tab,
    val item: DetailItem? = null,
    /** 直接打开设置页里的「进阶设置」（`screen=advanced`，截图用；对应 [App] 的 `initialAdvancedSettings`）。 */
    val advanced: Boolean = false,
    /**
     * 直接打开某个进阶子页（`screen=advanced:proxy`，[feedhub.app.settings.AdvancedSection.key]；
     * 对应 [App] 的 `initialAdvancedPage`）。null = 只看进阶的分组列表。
     */
    val advancedPage: String? = null,
)

/** 演示环境里的一条条目：平台 + 演示 id（就是 [feedhub.core.FeedItem.id]，如 `bili-0`）。 */
data class DetailItem(val platform: Platform, val id: String)

/** `screen=detail` 的默认条目：知乎的第 2 条（演示数据里它是图文，不是视频）。 */
const val DEFAULT_DETAIL_ITEM = "zhihu-1"

/** `screen=video` 的默认条目：B站的第 1 条（演示数据里它是视频，p == BILIBILI 时偶数条是视频）。 */
const val DEFAULT_VIDEO_ITEM = "bili-0"

/** 把启动参数解析成起始页，见 [StartScreen]。平台只认 [platforms] 里装了的。 */
fun startScreenOf(screen: String?, item: String? = null, platforms: PlatformCatalog = InstalledPlugins.catalog): StartScreen {
    val name = screen?.trim()?.lowercase().orEmpty()
    val wanted = item?.trim()?.takeIf { it.isNotEmpty() }
    return when (name) {
        // 只给 item 不给 screen：当作要开这条详情
        "" -> wanted?.let { StartScreen(Tab.HOME, detailItem(it, platforms)) } ?: StartScreen(Tab.HOME)
        "home" -> StartScreen(Tab.HOME)
        "following" -> StartScreen(Tab.FOLLOWING)
        "mine" -> StartScreen(Tab.MINE)
        "settings" -> StartScreen(Tab.SETTINGS)
        "advanced" -> StartScreen(Tab.SETTINGS, advanced = true)
        // `advanced:proxy` 这样直接开某一页（真机吞触摸，只能这样进）
        else -> if (name.startsWith("advanced:")) {
            val key = name.substringAfter(':').trim()
            val section = AdvancedSection.of(key)
                ?: throw IllegalArgumentException("未知的进阶子页：$key（认 ${AdvancedSection.keysText()}）")
            StartScreen(Tab.SETTINGS, advanced = true, advancedPage = section.key)
        } else {
            when (name) {
                "detail" -> StartScreen(Tab.HOME, detailItem(wanted ?: DEFAULT_DETAIL_ITEM, platforms))
                "video" -> StartScreen(Tab.HOME, detailItem(wanted ?: DEFAULT_VIDEO_ITEM, platforms))
                else -> throw IllegalArgumentException(
                    "未知的 screen=$screen（认 home / following / mine / settings / advanced / advanced:<子页> / detail / video）",
                )
            }
        }
    }
}

/** 演示条目 id → 平台 + id：`bili-0`、`xhs-3` 这样写；只有序号（`2`）时按第一个插件算。 */
fun detailItem(item: String, platforms: PlatformCatalog = InstalledPlugins.catalog): DetailItem {
    val hasPlatform = item.contains('-')
    val key = if (hasPlatform) item.substringBefore('-') else platforms.platforms.firstOrNull()?.key.orEmpty()
    val platform = platforms.find(key)
        ?: throw IllegalArgumentException("未知平台：$key（认 ${platforms.keysText()}）")
    return DetailItem(platform, if (hasPlatform) item else "${platform.key}-$item")
}
