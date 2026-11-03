package feedhub.app.image

import coil3.ComponentRegistry

/**
 * 按显示尺寸降采样解码，在 `ImageLoader.Builder.components { }` 里、动图解码器之后调用。
 *
 * - 桌面：Coil 用 Skia 先把整张原图解出来、再缩到显示尺寸；卡片上一张 4000 px 的照片只显示 300 px 宽，白解了十几倍的像素。
 *   这里让 JPEG / WebP 直接解到解码器原生支持的缩小档（见 desktopMain 的 `SampledImageDecoder`），显示尺寸、样子都不变。
 * - 安卓：系统的 `BitmapFactory` 本来就按 `inSampleSize` 降采样，Coil 已经在用，什么都不加。
 */
expect fun ComponentRegistry.Builder.addSampledImageDecoder(): ComponentRegistry.Builder
