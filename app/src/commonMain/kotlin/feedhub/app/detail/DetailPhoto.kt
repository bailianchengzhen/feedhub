package feedhub.app.detail

import androidx.compose.animation.core.animateFloatAsState
import androidx.compose.animation.core.tween
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Box
import androidx.compose.material3.MaterialTheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.layout.ContentScale
import coil3.compose.AsyncImage
import feedhub.app.feed.CardImageHint

/** 缩略图 → 原图的淡入时长。 */
internal const val DETAIL_IMAGE_FADE_MS = 180

/**
 * 详情里的一张图（正文单图、九宫格格子、视频封面、正文里嵌的卡片封面……）：
 *
 * **先画卡片上已经下好的那张缩略图**（[placeholder]，来自 [CardImageHint]），原图到了再淡入替换。
 * 两层一样大、同一个裁剪方式，缩略图一直垫在底下，所以换的时候不跳动、也不会从白 / 灰闪一下。
 *
 * 卡片上没有图（没有 [placeholder]）时就是一块和背景接近的占位色块，
 * 但转圈换成和背景接近的占位块"）。原来的写法一直是这样：Coil 的 `AsyncImage`
 * 在加载中什么都不画，外面那层 `surfaceVariant` 就是占位块；这里把它收进这个组件，两层的颜色一致。
 *
 * @param placeholder 卡片上**实际请求过**的缩略图地址（[CardImageHint.thumbnail]）；null / 和 [url] 相同就不垫。
 * @param onLoadedSize 原图加载完的像素尺寸（正文单图没有声明宽高时拿它把占位比例校正成真实比例；旧行为不变）。
 */
@Composable
fun DetailPhoto(
    url: String,
    contentDescription: String?,
    modifier: Modifier,
    contentScale: ContentScale = ContentScale.Crop,
    alignment: Alignment = Alignment.Center,
    placeholder: String? = null,
    placeholderColor: Color? = null,
    onLoadedSize: ((Size) -> Unit)? = null,
) {
    val boxColor = placeholderColor ?: MaterialTheme.colorScheme.surfaceVariant
    // 换地址（原文 / 译文、原图开关）时重新等一次：新图从透明淡入，底下的缩略图一直在
    var loaded by remember(url) { mutableStateOf(false) }
    val alpha by animateFloatAsState(if (loaded) 1f else 0f, tween(DETAIL_IMAGE_FADE_MS))
    // 时间线：这块图在不在组合里、原图到了没有。
    // 只在截图工具注册了探针时有用，正常界面就是两个 int 的加减（各一次）。
    DisposableEffect(Unit) {
        DetailDebug.imagesTotal++
        onDispose { DetailDebug.imagesTotal-- }
    }
    Box(modifier.background(boxColor)) {
        if (placeholder != null && placeholder != url) {
            AsyncImage(
                model = placeholder,
                contentDescription = null,
                contentScale = contentScale,
                alignment = alignment,
                modifier = Modifier.matchParentSize(),
            )
        }
        // 截图工具（`holdimages=1`）用：只画占位那一层，好截"刚进来有点糊"的样子
        if (!DetailDebug.holdFullImages) {
            AsyncImage(
                model = url,
                contentDescription = contentDescription,
                contentScale = contentScale,
                alignment = alignment,
                onSuccess = { state ->
                    if (!loaded) DetailDebug.imagesLoaded++
                    loaded = true
                    onLoadedSize?.invoke(state.painter.intrinsicSize)
                },
                modifier = Modifier.matchParentSize().alpha(alpha),
            )
        }
    }
}
