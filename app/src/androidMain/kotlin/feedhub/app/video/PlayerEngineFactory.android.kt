package feedhub.app.video

import android.content.Context
import okhttp3.OkHttpClient

/**
 * 安卓的播放引擎：**Media3 ExoPlayer**（见 [Media3PlayerEngine]）。
 *
 * 桌面上的那一份（`desktopMain` 的同名文件）是先 GStreamer（硬解）再 VLC 的退路；安卓只有这一个，
 * 起不来（理论上不会）就退回"这台设备还不能播放"的占位实现，不崩。
 *
 * [client] 是从 `commonMain` 传进来的**这个平台的 OkHttp 客户端**
 * （`graph.ctx.client(platform).build()`）：平台代理、fail-closed、Referer / UA 都在它里面，
 * 播放器的清单和分片都经它取。`null` = 没有组装好的图（测试 / 不该发生的路径），
 * 用一份不带平台规则的基础客户端，至少本机能直连的流还能放。
 */
actual fun createPlayerEngine(client: OkHttpClient?): PlayerEngine {
    val context = AndroidPlayback.appContext
        ?: return UnavailablePlayer("这台设备还不能播放：App 上下文还没准备好（播放器要在界面起来之后再建）")
    return runCatching { Media3PlayerEngine(context, client) }
        .getOrElse { e -> UnavailablePlayer("这台设备还不能播放：Media3 起不来（${e.message ?: e::class.simpleName}）") }
}

/**
 * 播放器要用的应用上下文：`ExoPlayer.Builder` / `PlayerView` 都要 Context，而 `createPlayerEngine(client)`
 * 的签名里只有 OkHttp 客户端（commonMain 的调用点拿不到 Context）。所以在**组装 `feedhub.app.AppGraph` 时**
 * 记一份（`androidGraph` 里设，见 `AndroidGraph.kt`）；之后的播放器都从这里取。
 */
internal object AndroidPlayback {
    @Volatile
    var appContext: Context? = null

    /** 记下应用上下文（重复调用没有副作用；只留 applicationContext，不持有 Activity）。 */
    fun remember(context: Context) {
        appContext = context.applicationContext
    }
}
