package feedhub.source.zhihu

import feedhub.core.BundledEmote

/**
 * 知乎表情（想法、评论里的 `[感谢]` `[赞同]`）。知乎的表情图只能从登录后的接口 `/api/v4/sticker-groups/…` 取，而且是内嵌的 data URI
 * （每张 4~16 KB）；放进信息流条目里太大，收藏 / 历史也要存它，所以默认组的图片随插件带（本模块的资源 `feedhub-emoji/zhihu/`），
 * 这里只知道"有哪些占位符、对应哪个文件"，地址是 [BundledEmote] 的 `feedhub-emoji:zhihu/<文件>`，由 app 的图片加载器取。
 * 信息流卡片摘要里的占位符靠它换成图片（`FeedItem.emotes`）；详情 / 评论里的表情仍走接口（`ZhihuSource.emotes`），是同一批图。
 */
object ZhihuSticker {
    const val PACK = "zhihu"

    /** 占位符（含方括号，如 `[感谢]`）→ 图片文件 id，不带扩展名；文件是资源 `feedhub-emoji/zhihu/<id>.png`。 */
    val table: Map<String, String> get() = ZHIHU_STICKER_TABLE

    /** 占位符 → [BundledEmote] 地址。 */
    val urls: Map<String, String> by lazy { ZHIHU_STICKER_TABLE.mapValues { (_, id) -> BundledEmote.url(PACK, "$id.png") } }
}
