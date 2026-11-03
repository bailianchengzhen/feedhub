package feedhub.app.ui

import coil3.ImageLoader
import coil3.Uri
import coil3.decode.DataSource
import coil3.decode.ImageSource
import coil3.fetch.FetchResult
import coil3.fetch.Fetcher
import coil3.fetch.SourceFetchResult
import coil3.request.Options
import feedhub.core.BundledEmote
import okio.Buffer
import java.io.FileNotFoundException

/**
 * 认 `feedhub-emoji:<表情包>/<文件>.png`（[BundledEmote]）的 Coil 取图器：从类路径资源 `feedhub-emoji/<表情包>/<文件>.png` 读字节。
 * 酷安 / 知乎表情官方没有公开图片地址，图片**随插件带**（插件模块的 `src/main/resources/feedhub-emoji/<表情包>/`，
 * 打进插件的 jar；安卓上 jar 里的 Java 资源也进 APK），source 模块只给出这样的地址（`Span.Emote.url`）。插件拆掉了，图跟着没了。
 *
 * 只进内存缓存：读资源本来就是本地的，不需要再抄一份进磁盘缓存（磁盘缓存是网络取图器自己管的，这个取图器不碰）。
 * 内存缓存的键是 Coil 默认的（地址字符串 + 目标尺寸），不需要自己的 Keyer。
 * 读不到（表里有、资源里没有，插件的测试会拦这种情况）就抛出异常，[coil3.compose.AsyncImage] 当作加载失败，行内显示文字代码。
 */
class BundledEmoteFetcher(private val ref: BundledEmote.Ref, private val options: Options) : Fetcher {
    override suspend fun fetch(): FetchResult {
        val bytes = readBytes(ref) ?: throw FileNotFoundException(resourcePath(ref))
        return SourceFetchResult(
            source = ImageSource(Buffer().apply { write(bytes) }, options.fileSystem),
            mimeType = "image/png",
            dataSource = DataSource.DISK,
        )
    }

    class Factory : Fetcher.Factory<Uri> {
        override fun create(data: Uri, options: Options, imageLoader: ImageLoader): Fetcher? =
            BundledEmote.parse(data.toString())?.let { BundledEmoteFetcher(it, options) }
    }

    companion object {
        /** 类路径上的资源路径。[BundledEmote.parse] 已经保证包名和文件名里只有简单字符。 */
        fun resourcePath(ref: BundledEmote.Ref) = "${BundledEmote.RESOURCE_DIR}/${ref.pack}/${ref.file}"

        /** 读图片字节；没有这个资源就是 null。 */
        fun readBytes(ref: BundledEmote.Ref): ByteArray? =
            BundledEmoteFetcher::class.java.classLoader?.getResourceAsStream(resourcePath(ref))?.use { it.readBytes() }
    }
}
