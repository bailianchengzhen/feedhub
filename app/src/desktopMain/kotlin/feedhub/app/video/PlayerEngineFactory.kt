package feedhub.app.video

import okhttp3.OkHttpClient

const val PLAYER_MISSING_MESSAGE =
    "需要安装 GStreamer（sudo apt install gstreamer1.0-plugins-good gstreamer1.0-plugins-bad gstreamer1.0-libav，硬解还要 mesa-va-drivers）或 VLC（sudo apt install vlc）"

/** 可选的播放引擎。 */
enum class PlayerKind { GST, VLC }

/**
 * 选引擎的顺序：`FEEDHUB_PLAYER=gst|vlc` 强制只用那一个（调试用，起不来就是"需要安装……"）；
 * 不强制就先 GStreamer（硬解），起不来再 VLC（软解）。认不出的值当没设。
 */
fun playerOrder(forced: String?): List<PlayerKind> = when (forced?.trim()?.lowercase()) {
    "gst", "gstreamer" -> listOf(PlayerKind.GST)
    "vlc" -> listOf(PlayerKind.VLC)
    else -> listOf(PlayerKind.GST, PlayerKind.VLC)
}

/** 按 [playerOrder] 依次试，第一个建得起来的就用；都不行返回 [PLAYER_MISSING_MESSAGE] 的占位。 */
internal fun createPlayerEngine(forced: String?, factories: Map<PlayerKind, () -> PlayerEngine>): PlayerEngine {
    for (kind in playerOrder(forced)) {
        try {
            return factories.getValue(kind)()
        } catch (e: Throwable) {
            // 找不到库 / 缺元素是 UnsatisfiedLinkError / IllegalStateException 之类；总之这个引擎起不来，试下一个，不崩
            System.err.println("播放引擎 $kind 初始化失败：$e")
        }
    }
    return UnavailablePlayer(PLAYER_MISSING_MESSAGE)
}

/**
 * [client] 是调用方给的这个平台的 OkHttp 客户端（安卓的 Media3 引擎拿它当数据源）。
 * 桌面两个引擎自己下载流（GStreamer 的 souphttpsrc / libvlc），数据源这边用不上，所以这里忽略它。
 */
actual fun createPlayerEngine(client: OkHttpClient?): PlayerEngine = createPlayerEngine(
    System.getenv("FEEDHUB_PLAYER"),
    mapOf(
        PlayerKind.GST to {
            // FEEDHUB_NO_GST=1 / FEEDHUB_NO_VLC=1：假装没装，看退路和"需要安装"提示用
            if (System.getenv("FEEDHUB_NO_GST") != null) error("FEEDHUB_NO_GST")
            // **没有开关**——
            // 生产路径就是这一份；Kotlin 原实现只在 desktopTest 里当对拍参照。
            RustGstEngine()
        },
        PlayerKind.VLC to {
            if (System.getenv("FEEDHUB_NO_VLC") != null) error("FEEDHUB_NO_VLC")
            VlcPlayerEngine()
        },
    ),
)
