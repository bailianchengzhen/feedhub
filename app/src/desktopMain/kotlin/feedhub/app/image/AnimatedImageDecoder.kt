package feedhub.app.image

import coil3.ComponentRegistry
import coil3.ImageLoader
import coil3.decode.DecodeResult
import coil3.decode.DecodeUtils
import coil3.decode.Decoder
import coil3.decode.ImageSource
import coil3.decode.SkiaImageDecoder
import coil3.fetch.SourceFetchResult
import coil3.request.Options
import coil3.request.maxBitmapSize
import coil3.size.Precision
import okio.Buffer
import okio.BufferedSource
import okio.FileSystem
import okio.use

/**
 * Coil 的动图解码器（桌面）：GIF / 动态 WebP 解成 [AnimatedImage]，别的格式（JPG / PNG / 静态 WebP……）不理，
 * 交给 Coil 原来的 Skia 解码器。
 *
 * 判断是不是动图看**文件开头的字节**（[AnimatedSniffer]），不看 URL 后缀和 Content-Type。
 * GIF 只有一帧时其实是静态图，这里照 Coil 的静态解码流程转给 [SkiaImageDecoder]，结果和没装这个解码器时一样。
 *
 * 大小的算法和 Coil 解静态图一样（按请求的大小、缩放方式、精度算出缩放后的尺寸，保持宽高比），
 * 这样内存缓存里的动图和静态图用同一套"够不够用"的判断。
 */
internal class AnimatedImageDecoder(
    private val source: ImageSource,
    private val options: Options,
    private val format: AnimatedFormat,
) : Decoder {

    override suspend fun decode(): DecodeResult {
        val bytes = source.source().use { it.readByteArray() }
        val image = AnimatedImage.create(bytes, format) { srcW, srcH -> targetSize(srcW, srcH) }
            ?: return SkiaImageDecoder(ImageSource(Buffer().write(bytes), FileSystem.SYSTEM), options).decode()
        return DecodeResult(image, isSampled = image.width < image.sourceWidth || image.height < image.sourceHeight)
    }

    private fun targetSize(srcW: Int, srcH: Int): Pair<Int, Int> {
        val dst = DecodeUtils.computeDstSize(srcW, srcH, options.size, options.scale, options.maxBitmapSize)
        val dstW = dst.first
        val dstH = dst.second
        var multiplier = DecodeUtils.computeSizeMultiplier(srcW, srcH, dstW, dstH, options.scale, options.maxBitmapSize)
        // 和静态图一样：只有要求精确大小时才放大
        if (options.precision == Precision.INEXACT) multiplier = multiplier.coerceAtMost(1.0)
        return (multiplier * srcW).toInt().coerceAtLeast(1) to (multiplier * srcH).toInt().coerceAtLeast(1)
    }

    class Factory : Decoder.Factory {
        override fun create(result: SourceFetchResult, options: Options, imageLoader: ImageLoader): Decoder? {
            val format = AnimatedSniffer.sniff(peek(result.source.source())) ?: return null
            return AnimatedImageDecoder(result.source, options, format)
        }

        /** 偷看开头几十个字节，不消耗数据源（Decoder.Factory 的约定）。 */
        private fun peek(source: BufferedSource): ByteArray {
            val p = source.peek()
            p.request(AnimatedSniffer.HEADER_BYTES.toLong())
            return p.readByteArray(minOf(AnimatedSniffer.HEADER_BYTES.toLong(), p.buffer.size))
        }
    }
}

actual fun ComponentRegistry.Builder.addAnimatedImageDecoder(): ComponentRegistry.Builder = add(AnimatedImageDecoder.Factory())
