package feedhub.app.image

import coil3.ComponentRegistry
import coil3.ImageLoader
import coil3.asImage
import coil3.decode.DecodeResult
import coil3.decode.DecodeUtils
import coil3.decode.Decoder
import coil3.decode.ImageSource
import coil3.decode.SkiaImageDecoder
import coil3.fetch.SourceFetchResult
import coil3.request.Options
import coil3.request.maxBitmapSize
import coil3.size.Precision
import coil3.size.pxOrElse
import okio.Buffer
import okio.BufferedSource
import org.jetbrains.skia.Bitmap
import org.jetbrains.skia.Canvas
import org.jetbrains.skia.Codec
import org.jetbrains.skia.ColorAlphaType
import org.jetbrains.skia.Data
import org.jetbrains.skia.EncodedImageFormat
import org.jetbrains.skia.EncodedOrigin
import org.jetbrains.skia.Image
import org.jetbrains.skia.ImageInfo
import org.jetbrains.skia.SamplingMode
import kotlin.math.ceil

/**
 * 按显示尺寸降采样解码 JPEG / WebP。
 *
 * Coil 在桌面上的默认解码（`SkiaImageDecoder`）是 `Image.makeFromEncoded` 整张解出来、再画进目标尺寸的位图。
 * 这里先算出和 Coil 一模一样的目标尺寸，然后让解码器直接解到"不小于目标的最小一档"
 * （JPEG：libjpeg-turbo 的 k/8 缩放；WebP：任意尺寸），再照 Coil 的做法画进目标尺寸。
 * 结果位图的尺寸和默认路径完全一样；摸底实测（101 张宽于 720 px 的样本）解码总耗时 −23%、解码时的像素量 −72%。
 *
 * 不走这条路、原样交给 Coil 默认解码的情况：不是 JPEG / WebP、多帧、带 EXIF 方向（默认路径会转正，这里不重复实现）、
 * 请求的是原图尺寸（看大图）、目标不比原图小、解码器不认这个缩放档或解码出错。
 */
internal class SampledImageDecoder(
    private val source: ImageSource,
    private val options: Options,
) : Decoder {
    override suspend fun decode(): DecodeResult {
        val bytes = source.source().use { it.readByteArray() }
        return decodeSampled(bytes, options) ?: SkiaImageDecoder(ImageSource(Buffer().write(bytes), options.fileSystem), options).decode()
    }

    class Factory : Decoder.Factory {
        override fun create(result: SourceFetchResult, options: Options, imageLoader: ImageLoader): Decoder? {
            if (DISABLED || !wantsSampling(options)) return null
            if (!isJpegOrWebp(peek(result.source.source()))) return null
            return SampledImageDecoder(result.source, options)
        }

        private fun peek(source: BufferedSource): ByteArray {
            val p = source.peek()
            p.request(HEADER_BYTES)
            return p.readByteArray(minOf(HEADER_BYTES, p.buffer.size))
        }
    }

    companion object {
        private const val HEADER_BYTES = 12L

        /** `-Dfeedhub.sampledDecode=false` 关掉（对照测量 / 万一哪种图解坏了时的退路）。 */
        private val DISABLED = System.getProperty("feedhub.sampledDecode") == "false"

        /** 有明确的目标像素尺寸才降采样（`Size.ORIGINAL` / 某一边不定 = 要原图，比如看大图）。 */
        internal fun wantsSampling(options: Options): Boolean =
            options.size.width.pxOrElse { -1 } > 0 && options.size.height.pxOrElse { -1 } > 0

        internal fun isJpegOrWebp(head: ByteArray): Boolean {
            if (head.size >= 3 && head[0] == 0xFF.toByte() && head[1] == 0xD8.toByte() && head[2] == 0xFF.toByte()) return true
            return head.size >= 12 && String(head, 0, 4, Charsets.US_ASCII) == "RIFF" && String(head, 8, 4, Charsets.US_ASCII) == "WEBP"
        }

        /** 和 Coil 默认解码（`Bitmap.makeFromImage`）一样的目标尺寸：按 scale 适配、不放大（INEXACT）、不超过最大位图尺寸。 */
        internal fun targetSize(srcW: Int, srcH: Int, options: Options): Pair<Int, Int> {
            val dstW = options.size.width.pxOrElse { srcW }
            val dstH = options.size.height.pxOrElse { srcH }
            var m = DecodeUtils.computeSizeMultiplier(srcW, srcH, dstW, dstH, options.scale, options.maxBitmapSize)
            if (options.precision == Precision.INEXACT) m = m.coerceAtMost(1.0)
            // Coil 是截断取整（168.75 → 168）
            return (m * srcW).toInt().coerceAtLeast(1) to (m * srcH).toInt().coerceAtLeast(1)
        }

        /** 解码器直接解到的尺寸：JPEG 取 k/8（k = 1..8）里不小于目标的最小一档；WebP 直接是目标。 */
        internal fun sampledSize(format: EncodedImageFormat, srcW: Int, srcH: Int, tw: Int, th: Int): Pair<Int, Int> {
            if (format != EncodedImageFormat.JPEG) return tw to th
            val k = (1..8).firstOrNull { ceil(srcW * it / 8.0).toInt() >= tw && ceil(srcH * it / 8.0).toInt() >= th } ?: 8
            return ceil(srcW * k / 8.0).toInt() to ceil(srcH * k / 8.0).toInt()
        }

        /** 降采样解码；不适用或失败给 null（调用方退回默认解码）。 */
        internal fun decodeSampled(bytes: ByteArray, options: Options): DecodeResult? {
            val codec = runCatching { Codec.makeFromData(Data.makeFromBytes(bytes)) }.getOrNull() ?: return null
            codec.use { c ->
                val format = c.encodedImageFormat
                if (format != EncodedImageFormat.JPEG && format != EncodedImageFormat.WEBP) return null
                if (c.frameCount > 1 || c.encodedOrigin != EncodedOrigin.TOP_LEFT) return null
                val (tw, th) = targetSize(c.width, c.height, options)
                if (tw >= c.width || th >= c.height) return null
                val (sw, sh) = sampledSize(format, c.width, c.height, tw, th)
                if (sw >= c.width && sh >= c.height) return null
                val sampled = Bitmap()
                try {
                    if (!sampled.allocPixels(ImageInfo.makeN32(sw, sh, if (c.isOpaque) ColorAlphaType.OPAQUE else ColorAlphaType.PREMUL))) return null
                    if (runCatching { c.readPixels(sampled) }.isFailure) return null
                    val out = if (sw == tw && sh == th) {
                        sampled.setImmutable()
                        sampled
                    } else {
                        // 和 Coil 默认解码同样的最后一步：画进目标尺寸（同样的采样方式）
                        Bitmap().also { dst ->
                            dst.allocN32Pixels(tw, th)
                            Image.makeFromBitmap(sampled).use { img ->
                                Canvas(dst).use { cv ->
                                    cv.drawImageRect(img, 0f, 0f, sw.toFloat(), sh.toFloat(), 0f, 0f, tw.toFloat(), th.toFloat(), SamplingMode.DEFAULT, null, true)
                                }
                            }
                            sampled.close()
                            dst.setImmutable()
                        }
                    }
                    return DecodeResult(out.asImage(), isSampled = true)
                } catch (e: Throwable) {
                    sampled.close()
                    return null
                }
            }
        }
    }
}

actual fun ComponentRegistry.Builder.addSampledImageDecoder(): ComponentRegistry.Builder = add(SampledImageDecoder.Factory())
