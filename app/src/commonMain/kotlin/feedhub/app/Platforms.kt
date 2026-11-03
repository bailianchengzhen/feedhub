package feedhub.app

import androidx.compose.runtime.staticCompositionLocalOf
import feedhub.core.plugin.PlatformCatalog
import feedhub.plugins.InstalledPlugins

/**
 * 界面里的平台目录：有哪些平台（顺序 = 界面顺序）、每个平台的颜色 / 能力 / 叫法都从这里问，
 * **不按平台 id 写 if / when**。
 *
 * 默认是这次构建装好的插件（[InstalledPlugins]）；[App] 用 `AppGraph.platforms` 提供，测试 / 演示可以换成自己的。
 */
val LocalPlatforms = staticCompositionLocalOf<PlatformCatalog> { InstalledPlugins.catalog }
