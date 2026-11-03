package feedhub.app.detail

import feedhub.core.Block
import feedhub.core.Comment
import feedhub.core.FeedItem
import feedhub.core.ItemKind
import feedhub.core.RichText

/**
 * 知乎的**问题版面**：点开回答 / 问题，上面是问题（标题 + 描述 + 提问者），
 * 评论区放这个问题下的回答，推过来的那条置顶；每条回答是一"楼"，它的评论当楼中楼。
 * 请求和解析在 Rust（`feedhub-core/src/zhihu/qa.rs`），界面这边只换几处文字、给长回答加「展开全文」、
 * 回答楼层滑到屏幕上时再取几条评论当预览。
 */
internal fun isQuestionView(item: FeedItem): Boolean =
    item.platform.key == ZHIHU_KEY && (item.kind == ItemKind.ANSWER || item.kind == ItemKind.QUESTION)

/** 评论区里这一"楼"是一条知乎回答（楼 id 是 `answer:<id>`）。 */
internal fun isAnswerFloor(comment: Comment): Boolean =
    comment.platform.key == ZHIHU_KEY && comment.id.startsWith("answer:")

/** 问题版面的评论区标题："回答 N"（别的平台是"评论 N"）。 */
internal fun answersHeaderTitle(commentsTitle: String): String = commentsTitle.replaceFirst("评论", "回答")

/**
 * 长回答先折叠：只留开头几块（字数到 [FOLD_CHARS] 或块数到 [FOLD_BLOCKS] 为止，图最多一张），
 * 剩下的点「展开全文」。返回 null = 不用折叠。
 */
internal fun foldedAnswer(content: RichText): RichText? {
    val blocks = content.blocks
    if (blocks.size <= FOLD_BLOCKS && content.textOnly().length <= FOLD_CHARS && blocks.count { it is Block.Image } <= 1) return null
    val kept = ArrayList<Block>()
    var chars = 0
    var images = 0
    for (block in blocks) {
        if (kept.size >= FOLD_BLOCKS || chars >= FOLD_CHARS) break
        if (block is Block.Image) {
            if (images >= 1) break
            images++
        }
        kept += block
        chars += RichText(listOf(block)).textOnly().length
    }
    if (kept.isEmpty()) kept += blocks.first()
    return if (kept.size >= blocks.size) null else RichText(kept)
}

/** 回答楼层折叠时留几块 / 多少字。 */
private const val FOLD_BLOCKS = 4
private const val FOLD_CHARS = 280

/** 回答楼层下面预览几条评论（多的点「共 N 条评论」看全部）。 */
internal const val ANSWER_PREVIEW_COMMENTS = 3

private const val ZHIHU_KEY = "zhihu"
