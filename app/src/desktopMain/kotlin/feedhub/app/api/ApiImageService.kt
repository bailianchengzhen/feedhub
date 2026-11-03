package feedhub.app.api

import coil3.BitmapImage
import coil3.ImageLoader
import coil3.PlatformContext
import coil3.decode.DataSource
import coil3.network.HttpException
import coil3.request.CachePolicy
import coil3.request.ErrorResult
import coil3.request.ImageRequest
import coil3.request.SuccessResult
import feedhub.core.RequestScope
import org.jetbrains.skia.Bitmap
import org.jetbrains.skia.Canvas
import org.jetbrains.skia.EncodedImageFormat
import org.jetbrains.skia.Image
import org.jetbrains.skia.Rect
import org.jetbrains.skia.SamplingMode

/**
 * `/image` 的取图 / 出图。
 *
 * 三件事：
 * 1. **图由 App 去取**：走 App 自己的那个 Coil `ImageLoader`（`App.kt` 里 `setSingletonImageLoaderFactory` 建的那一份）——
 *    于是和 App 刷流取图走**同一条路**：同一个加载器 / 同一个磁盘缓存、浏览器 UA、按域名的代理、fail-closed
 *    全部照旧生效（图片客户端本来就不带 Cookie / Referer，和 App 里一样），agent 永远碰不到图片地址；
 * 2. **重复取不再发请求**：Coil 的**磁盘缓存**就是 App 刷流时用的那一个（同一个 loader、同一个目录），
 *    命中时 `DataSource` 不是 NETWORK → 这次调用**一个请求都不算**（`X-Feedhub-Requests: 0`）。
 *    只有真从网络取的那次才 `call.record()` 记一笔；
 * 3. **出图**：按 `maxw` 缩（不放大）、长图按段切，统一编码成 JPEG（质量 [JPEG_QUALITY]）。
 *    接口不往磁盘写任何自己的文件（缓存是 App 的图片缓存，由 Coil 自己管）。
 *
 * 内存里的那张图整张解出来再缩（和 App 看大图时一样）；超大的图解不动就报错，不会把 App 拖死。
 */
internal class ApiImageService(
    private val loader: () -> ImageLoader,
) {
    /** 取一张图并出图；[segment] 只对长图有意义（null = 第 0 段 / 整张）。 */
    suspend fun render(url: String, maxWidth: Int, segment: Int?, call: RequestScope): RenderedImage {
        // 只取网页图片：正文里 `file:///…` 的图会被 Coil 的本地文件 fetcher 读出来交给 agent
        val scheme = url.trim().substringBefore(':', "").lowercase()
        if (scheme != "http" && scheme != "https") throw ApiFailure(ApiError.BAD_INPUT, "只能取 http / https 的图片")
        val request = ImageRequest.Builder(PlatformContext.INSTANCE)
            .data(url)
            // 不占 App 的内存缓存（那是给界面用的）：磁盘缓存照样读、照样写
            .memoryCachePolicy(CachePolicy.DISABLED)
            .build()
        val image = when (val result = execute(request)) {
            is SuccessResult -> {
                // 只有真的从网络取了才算一个请求；磁盘缓存命中是 0（见类注释）
                if (result.dataSource == DataSource.NETWORK) call.record()
                result.image
            }

            is ErrorResult -> {
                // 失败也分两种：真去发了请求（CDN 回错 / 连不上）要算进预算；
                // 缓存里的坏了、解码失败那种没有请求，不算
                if (result.throwable is HttpException || result.throwable is java.io.IOException) call.record()
                throw result.throwable.toImageFailure()
            }
        }
        val w = image.width
        val h = image.height
        if (w <= 0 || h <= 0) throw ApiFailure(ApiError.PLATFORM_ERROR, "这张图解码出来是空的")

        val (tw, th) = ImageGeometry.scaled(w, h, maxWidth)
        val plan = ImageGeometry.segments(tw, th)
        val index = segment ?: 0
        if (index >= plan.count) {
            throw ApiFailure(
                ApiError.BAD_INPUT,
                if (plan.count <= 1) {
                    "这张图不是长图，不分段（seg 不用传）"
                } else {
                    "seg 越界：这张图只有 ${plan.count} 段（0..${plan.count - 1}）"
                },
            )
        }
        val top = if (segment == null) 0 else plan.top(index)
        val segHeight = if (segment == null) plan.heightAt(0) else plan.heightAt(index)
        val bytes = encodeJpeg(image, w, h, tw, segHeight, top)
        return RenderedImage(
            bytes = bytes,
            width = tw,
            height = segHeight,
            fullWidth = tw,
            fullHeight = th,
            segments = plan.count,
            segment = if (plan.count > 1) index else 0,
        )
    }

    private suspend fun execute(request: ImageRequest) = try {
        loader().execute(request)
    } catch (e: ApiFailure) {
        throw e
    } catch (e: Throwable) {
        throw e.toImageFailure()
    }

    /**
     * 把 [image]（宽 [width] × 高 [height]）的第 [top] 行起、高 [outHeight] 的那一段，缩到宽 [outWidth]，编成 JPEG。
     *
     * 走的是 Skia：`BitmapImage` 能拿到 skia 的 `Image`，就用 `drawImageRect(MITCHELL)` 一次缩到位（缩小的图边缘不发毛）；
     * 动图那种不是 `BitmapImage` 的（GIF / 动态 WebP），退回 `Image.draw` + 画布缩放（画当前这一帧）。
     */
    private fun encodeJpeg(image: coil3.Image, width: Int, height: Int, outWidth: Int, outHeight: Int, top: Int): ByteArray {
        val scale = outWidth.toFloat() / width
        val dst = Bitmap()
        if (!dst.allocN32Pixels(outWidth, outHeight, false)) {
            throw ApiFailure(ApiError.INTERNAL, "图片太大，分配不出 ${outWidth}x$outHeight 的画布")
        }
        try {
            Canvas(dst).use { canvas ->
                // JPEG 没有透明通道：先铺白（PNG 的透明区域否则会变黑）
                canvas.clear(WHITE)
                val skiaImage = (image as? BitmapImage)?.bitmap?.let { Image.makeFromBitmap(it) }
                if (skiaImage != null) {
                    skiaImage.use { src ->
                        val srcTop = (top / scale).coerceIn(0f, (height - 1).toFloat())
                        val srcHeight = (outHeight / scale).coerceIn(1f, height - srcTop)
                        canvas.drawImageRect(
                            src,
                            Rect.makeXYWH(0f, srcTop, width.toFloat(), srcHeight),
                            Rect.makeXYWH(0f, 0f, outWidth.toFloat(), outHeight.toFloat()),
                            SamplingMode.MITCHELL,
                            null,
                            true,
                        )
                    }
                } else {
                    canvas.save()
                    canvas.scale(scale, scale)
                    // top 是**缩放后**坐标：translate 在已经 scale 过的坐标系里，所以要除回去
                    canvas.translate(0f, -(top / scale))
                    image.draw(canvas)
                    canvas.restore()
                }
            }
            dst.setImmutable()
            val encoded = Image.makeFromBitmap(dst).use { it.encodeToData(EncodedImageFormat.JPEG, JPEG_QUALITY) }
                ?: throw ApiFailure(ApiError.INTERNAL, "JPEG 编码失败")
            return encoded.use { it.bytes }
        } catch (e: ApiFailure) {
            throw e
        } catch (e: Throwable) {
            throw ApiFailure(ApiError.INTERNAL, "缩图失败：${e.message ?: e::class.simpleName}")
        } finally {
            dst.close()
        }
    }

    companion object {
        /** JPEG 质量（和定稿一样 ~85）。 */
        const val JPEG_QUALITY: Int = 85

        /** 没有透明通道的底色（白色）。 */
        private const val WHITE: Int = 0xFFFFFFFF.toInt()
    }
}

/** 一次 `/image` 的结果：JPEG 字节 + 响应头里那几个尺寸 / 段数。 */
internal class RenderedImage(
    val bytes: ByteArray,
    /** 返回的这一段有多宽（= 缩放后的宽）。 */
    val width: Int,
    /** 返回的这一段有多高。 */
    val height: Int,
    /** 整张图（缩放后）多宽。 */
    val fullWidth: Int,
    /** 整张图（缩放后）多高。 */
    val fullHeight: Int,
    /** 这张图一共分几段（1 = 没切）。 */
    val segments: Int,
    /** 这是第几段（0 起；不分段时是 0）。 */
    val segment: Int,
)

/**
 * 取图失败 → 接口错误码：
 * 平台 / CDN 明确回了 HTTP 错误（签名过期、403、404）算 `platform_error`（多半是地址变了，别重试）；
 * 连不上 / 超时算 `network`。**这两条都不带原始异常的消息**：OkHttp / Coil 的消息里可能带着图片地址
 * （有的 CDN 地址本身就是带签名的），接口的任何输出里都不能出现它。
 */
internal fun Throwable.toImageFailure(): ApiFailure = when (this) {
    is ApiFailure -> this
    is HttpException -> ApiFailure(
        ApiError.PLATFORM_ERROR,
        "图片服务器回了 HTTP ${response.code}（地址可能过期了，重取一次详情再试）",
    )

    is java.io.IOException -> ApiFailure(
        ApiError.NETWORK,
        "取图时网络出错（连不上 / 超时；地址可能有有效期，重取一次详情再试）",
    )

    else -> ApiFailure(ApiError.INTERNAL, "取图失败：${this::class.simpleName ?: "未知错误"}")
}
