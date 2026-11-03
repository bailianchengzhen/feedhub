package feedhub.source.douyin

import feedhub.core.ApiException
import feedhub.core.Block
import feedhub.core.Emotes
import feedhub.core.FeedDetail
import feedhub.core.ParseException
import feedhub.core.Platform
import feedhub.core.RichText
import feedhub.core.arr
import feedhub.core.bool
import feedhub.core.int
import feedhub.core.long
import feedhub.core.obj
import feedhub.core.text
import feedhub.core.toHttps
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive

/**
 * 原生详情：`GET /aweme/v1/web/aweme/detail/` 的响应（纯函数，`DouyinDetailTest` 喂 fixtures）。
 *
 * 响应里的作品在 `aweme_detail`（**对象**，不像推荐流那样是一段 JSON 字符串）。
 * 作品本身的映射复用 [DouyinParser.item]，保证详情里的 `id` / `key` 和信息流里同一条一致（收藏 / 历史按 key 记）。
 *
 * ## 正文
 *
 * 抖音没有标题字段，`desc` 就是文案，放一个段落（`FeedItem.summary` 也是它）。文案里的文字表情代码
 * （`[赞]`，
 * 表情代码在详情里是字面文本。`desc` 里的 `#话题` 现在**只是文字**：`text_extra` 给了 `hashtag_name` / `start` / `end`，
 * 但站内话题页我们不做原生详情、官方话题页的地址形态也没有实测出处，所以不编 `Span.Link.target`。
 * 图文作品把 `images[]` 按 `Block.Image` 放进正文（带原图 `url_list` 里的第二张，如果有）。
 *
 * ## 详情里附带的评论
 *
 * `aweme_detail.comment_list` 是平台在详情里捎带的几条评论，**不用它**：评论一律走 `comment/list`（分页、有游标），
 * 免得两处的顺序 / 分页口径打架。
 */
internal object DouyinDetail {
    private val P = DOUYIN

    /** @param emotes 文案里的文字代码 → 图片地址（[DouyinSource] 按"文案里有没有 `[xx]`"决定要不要去取表）。 */
    fun detail(json: JsonObject, id: String, emotes: Map<String, String> = emptyMap()): FeedDetail {
        val aweme = json.obj("aweme_detail")
            ?: throw notFound(json, id)
        val item = DouyinParser.item(aweme, emotes)
            ?: throw ParseException(P, "aweme/detail 里的作品映射不出条目（id=$id）")
        return FeedDetail(
            item = item,
            body = body(aweme, emotes),
            video = DouyinVideo.info(aweme),
            // 关注 / 取关当前不做，所以不给 FollowState
            follow = null,
            related = emptyList(),
        )
    }

    /**
     * 作品看不到时服务端给的是 `filter_detail` + `status_msg`（已删除 / 私密 / 需要登录）。
     * 把平台的话原样带出来，不要假装是解析失败。[DouyinSource.play] 也用它（`play` 要拿同一个作品对象）。
     */
    internal fun notFound(json: JsonObject, id: String): ApiException {
        val reason = json.obj("filter_detail")?.text("detail_msg")
            ?: json.obj("filter_detail")?.text("reason")
            ?: json.text("status_msg")
        return ApiException(P, "no_aweme", "拿不到作品 $id 的详情" + if (reason.isNullOrBlank()) "" else "：$reason")
    }

    private fun body(aweme: JsonObject, emotes: Map<String, String>): RichText {
        val blocks = mutableListOf<Block>()
        aweme.text("desc")?.takeIf { it.isNotBlank() }?.let { desc ->
            // 文案里的 `[赞]` 这种文字表情和卡片一样画成行内小图（表里没有的保持文字）
            blocks += Block.Paragraph(Emotes.split(desc, emotes))
        }
        blocks += aweme.arr("images").orEmpty().mapNotNull { (it as? JsonObject)?.let(::image) }
        return RichText(blocks)
    }

    private fun image(o: JsonObject): Block.Image? {
        val urls = o.arr("url_list").orEmpty()
            .mapNotNull { (it as? JsonPrimitive)?.content?.takeIf { u -> u.isNotBlank() } }
        val url = urls.firstOrNull() ?: return null
        return Block.Image(
            url = url.toHttps(),
            width = o.int("width"),
            height = o.int("height"),
            // 第二张通常是原图 / 无损图（`url_list` 里前两张同尺寸不同清晰度）
            original = urls.getOrNull(1)?.toHttps(),
        )
    }

    /** 详情响应里作品自己带的字段（`is_ads` 之类）也照信息流一样丢掉非内容卡片。 */
    fun isAd(aweme: JsonObject): Boolean = aweme.bool("is_ads") == true
}
