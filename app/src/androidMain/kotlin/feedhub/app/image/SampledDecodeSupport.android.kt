package feedhub.app.image

import coil3.ComponentRegistry

/** 安卓上 Coil 的 `BitmapFactory` 解码本来就降采样，不用加。 */
actual fun ComponentRegistry.Builder.addSampledImageDecoder(): ComponentRegistry.Builder = this
