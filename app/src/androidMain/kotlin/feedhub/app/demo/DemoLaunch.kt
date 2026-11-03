package feedhub.app.demo

import feedhub.app.AppGraph
import feedhub.app.DetailItem
import feedhub.core.DetailTarget
import feedhub.core.FeedItem
import feedhub.core.NativeDetailSource
import kotlinx.coroutines.runBlocking

/**
 * 演示环境里按 [DetailItem] 取一条完整的 [FeedItem]，给 debug 的启动参数用（见 [feedhub.app.StartScreen]
 * 和 `:androidApp` 的 `DebugStartActivity`）。
 *
 * 走的是演示 Source 的 `detail`（[DemoSource]，纯本地拼数据，**一个请求也不发**），和桌面截图工具
 * `dev/Screenshot.kt` 里 `screen=detail:<平台>:<id>` 那条路一个写法。
 *
 * 序号超出范围不报错：演示源自己会 `coerceIn` 到 0..5（见 `DemoItems.index`），这里不重复一遍。
 */
fun androidDemoItem(graph: AppGraph, item: DetailItem): FeedItem {
    val source = graph.source(item.platform) as? NativeDetailSource
        ?: error("${item.platform.key} 的演示源没有原生详情")
    return runBlocking { source.detail(item.id).item }.copy(detail = DetailTarget.Native(item.id))
}
