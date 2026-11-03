package feedhub.core.plugin

import feedhub.core.FeedCookieJar
import feedhub.core.FeedItem
import feedhub.core.KeyValueStore
import feedhub.core.MemoryCookieJar
import feedhub.core.MemoryKeyValueStore
import feedhub.core.Platform
import feedhub.core.Source
import feedhub.core.SourceContext
import okhttp3.Interceptor
import java.io.File

/**
 * 插件自带的**不联网**演示环境：截图工具（`./gradlew :app:screenshot -Pargs="… fake=<名字>"`）用它把整个 App
 * 跑在录制好的响应上，看界面不打接口。
 *
 * 放在插件模块的 `demo` 源码集里（`src/demo/kotlin/`，用 `META-INF/services/feedhub.core.plugin.PlatformDemo` 登记），
 * **只进开发工具的类路径**（截图 / 调试入口），不进 App 的发布包和 APK。插件拆掉了，它的 `fake=` 跟着没了。
 * 没有演示环境的插件不用写。
 */
interface PlatformDemo {
    /** 截图工具 `fake=` 认的名字，第一个是"登录状态"的主演示（例如 `xhs`、`xhs-out`、`adult`）。 */
    val modes: List<String>

    /** 按 [mode] 搭一套假环境。只在截图工具里调，可以读仓库里的夹具文件（工作目录是仓库根）。 */
    fun open(mode: String, env: DemoEnv): DemoWorld
}

/** 截图工具给演示环境的东西。 */
class DemoEnv(
    /** 截图工具的全部参数（插件自己认的，如 `videourl=`，也在里面）。 */
    val options: Map<String, String>,
    /** 截图工具的 `videourl=` / `videourlfile=`：要用真实的播放地址看效果时给（那样会联网取视频数据）；平时是 null。 */
    val videoUrl: String?,
    /** 本机现生成的竖屏测试视频（540x960 AVC，ffmpeg 生成、有缓存）：视频的播放地址换成它，不访问 CDN。 */
    val portraitVideo: () -> File,
)

/**
 * 一套假环境：这个平台用真的 Source（真的解析、真的签名），只是底下的网络换成 [network]；
 * 别的平台由截图工具放占位（未登录、空信息流）。
 */
class DemoWorld(
    val platform: Platform,
    /** 这个平台的 Source（一般就是插件描述的工厂，也可以包一层，比如往推荐流里埋演示条目）。 */
    val source: (SourceContext) -> Source,
    /** 接口请求的假网络：挂在共用的客户端上（视频中转也用它，媒体请求要放过去）。不认识的请求一律 `IOException`。 */
    val network: Interceptor,
    /** 图片客户端的假网络（现画占位图）。 */
    val images: Interceptor,
    /** Cookie（登录状态的演示放占位值，**不是任何人的凭据**）。 */
    val cookies: FeedCookieJar = MemoryCookieJar(),
    val store: KeyValueStore = MemoryKeyValueStore(),
    /**
     * 把这个平台点亮在「走代理的平台」里（= 默认的系统代理，离线时就是直连）。抖音用：音画一体的 mp4 由播放器
     * 直接取 `file://` 的测试视频，不经本机中转（中转取不了 `file://`）。
     */
    val proxied: Boolean = false,
    /** 截图工具 `adultdetail=1` 直接打开的条目（小红书 `fake=adult` 埋的那条被标 18+ 的）；没有就是 null。 */
    val hiddenItem: FeedItem? = null,
)
