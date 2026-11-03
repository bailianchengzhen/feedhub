package feedhub.app.image

import coil3.ImageLoader
import coil3.PlatformContext
import coil3.disk.DiskCache
import coil3.network.NetworkFetcher
import coil3.request.crossfade
import feedhub.app.AppGraph
import feedhub.app.ui.BundledEmoteFetcher
import okio.Path.Companion.toOkioPath

/**
 * App 的图片加载器（Coil）——**只有这一份配置**。
 *
 * 两处用它，必须是**同一个单例**：
 * 1. 界面：`App()` 的 `setSingletonImageLoaderFactory`；
 * 2. 对外接口的 `/image`（`DesktopAppApi` 的默认加载器）。
 *
 * 为什么必须同一份：`/image` 的"重复取不再发请求"靠的就是 **App 的图片磁盘缓存**（`graph.imageCacheDir`，
 * 桌面 `~/.cache/feedhub/images`）——用户刷流时看过的图，agent 再要就是 0 个请求；代理 / UA 也走同一份
 * `graph.imageFetch`。两个 loader 各开一份 `DiskCache` 指向同一个目录是不行的（同一个 journal 两处写）。
 */
fun appImageLoader(context: PlatformContext, graph: AppGraph): ImageLoader = ImageLoader.Builder(context)
    .components {
        // Coil 的单例整个进程只建一次，但安卓上 Activity 重建（返回键退出再进、系统回收界面）会换一份新的
        // AppGraph：要是捕获第一份 graph，图片就一直走旧 graph 的代理——旧的内置核心早停了，fail-closed
        // 之下走代理的平台（YouTube 等）图片全挂、接口却正常。所以**每次请求现读最新那份**。
        // 实现是 `feedhub.app.image.CoilImageClient`：后面那个 `ImageFetch` 就是"带浏览器 UA + 按域名选代理"
        // 的那一层。
        add(NetworkFetcher.Factory(networkClient = { CoilImageClient { (AppGraph.current ?: graph).imageFetch } }))
        add(BundledEmoteFetcher.Factory()) // feedhub-emoji:…（酷安 / 知乎表情，图片随插件带，在类路径资源里）
        addAnimatedImageDecoder() // GIF / 动态 WebP 按帧解码，见 feedhub.app.image
        addSampledImageDecoder() // 静态 JPEG / WebP 直接按显示尺寸降采样解（桌面），见 feedhub.app.image
    }
    .apply {
        graph.imageCacheDir?.let { dir ->
            diskCache { DiskCache.Builder().directory(dir.toOkioPath()).maxSizeBytes(512L * 1024 * 1024).build() }
        }
    }
    .crossfade(true)
    // 性能记录 / Perfetto：图片解码的异步切片（见 feedhub.app.perf.PerfImageTrace；不写日志、不受开关影响）
    .eventListener(feedhub.app.perf.PerfImageTrace)
    .build()
