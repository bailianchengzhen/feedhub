package feedhub.app.perf

import coil3.decode.DecodeResult
import coil3.EventListener
import coil3.decode.Decoder
import coil3.request.Options
import coil3.request.ImageRequest
import java.util.Collections
import java.util.IdentityHashMap

/**
 * 图片解码的追踪标记（任务 §6："图片解码（看现在用的图片加载器在哪能挂）"）。
 *
 * 现在的图片加载器是 Coil 3（`App.kt` 里 `setSingletonImageLoaderFactory`）。它自带 `EventListener`，在
 * `decodeStart` / `decodeEnd` 之间就是真正的解码（换图片格式、大小、动图也都在这之间），不用改 Coil 的解码器，
 * 也不碰我们自己的 `SampledImageDecoder` / `AnimatedImageDecoder`（那两个只在桌面接）。
 *
 * 解码可能在 Coil 的 IO 线程上、`decodeStart` / `decodeEnd` 甚至不保证同一个线程，所以用**异步切片**
 * （`Trace.beginAsyncSection`），cookie 按请求对象配对（`IdentityHashMap`：Coil 的 `ImageRequest` 是 data class，
 * 两个一样的请求不算同一个）。
 *
 * 这些标记**不受"性能记录"开关影响**：没开也照样打（没接 Perfetto 时是空操作），这样以后拿 Perfetto 录就有用；
 * 只有写 jsonl 的事件才受开关控制。
 */
object PerfImageTrace : EventListener() {
    private val cookies: MutableMap<ImageRequest, Int> = Collections.synchronizedMap(IdentityHashMap())

    override fun decodeStart(request: ImageRequest, decoder: Decoder, options: Options) {
        cookies[request] = traceBeginAsync("image.decode")
    }

    override fun decodeEnd(request: ImageRequest, decoder: Decoder, options: Options, result: DecodeResult?) {
        val cookie = cookies.remove(request) ?: return
        traceEndAsync("image.decode", cookie)
    }
}
