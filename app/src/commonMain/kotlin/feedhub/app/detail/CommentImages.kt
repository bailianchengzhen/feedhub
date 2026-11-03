package feedhub.app.detail

import androidx.compose.ui.unit.dp
import feedhub.present.RustPresentImages

/**
 * 评论 / 楼中楼里的图片怎么排（规则与地址改写在 Rust，枚举与尺寸常量保留给 UI）。
 */
enum class RichImageMode { BODY, COMMENT, PREVIEW }

/** 预览框里小图的边长：约两行 13sp 文字高，一行四张还放得下，不把评论区撑长。 */
val CommentThumbSize = 56.dp

/** 预览框里一行最多画几张。 */
const val CommentThumbMax = 4

/** 预览框里一行画几张（≤ [CommentThumbMax]）。 */
fun commentThumbCells(count: Int): Int = RustPresentImages.commentThumbCells(count)

/** 预览框里没画出来的张数。 */
fun commentThumbMore(count: Int): Int = RustPresentImages.commentThumbMore(count)

/**
 * 评论里的图片该请求哪个地址：走缩略图改写规则，宽度夹取在 200..1920。
 */
fun commentImageUrl(url: String, widthPx: Int): String =
    RustPresentImages.commentImageUrl(url, widthPx)

/** 批量改写评论图片地址。 */
fun commentImageUrlBatch(urls: List<String>, widthPxs: List<Int>): List<String> =
    RustPresentImages.commentImageUrlBatch(urls, widthPxs)
