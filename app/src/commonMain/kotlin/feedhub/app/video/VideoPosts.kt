package feedhub.app.video

import feedhub.core.FeedItem
import feedhub.core.ItemKind
import feedhub.core.Media
import feedhub.present.RustPresent

/**
 * 这条内容整条就是视频吗：是的话详情页走视频详情（播放器在顶上，下面简介 + 评论），不是的话走图文详情。
 *
 * 看 [FeedItem.kind] 是视频，或者媒体里第一项是视频——贴吧的视频帖在信息流里是普通帖子（kind 是 POST），
 * 只有 [FeedItem.media] 里带一个 [Media.Video]；老的收藏 / 历史条目也是这样存的，所以不能只看 kind。
 * 图文帖里夹着视频的（楼层里的视频、回答里的视频）不算：`media` 里没有视频，它们在正文里原地播放（[InlineVideo]）。
 * 能不能真的播放还要看平台的 Source 是不是 [feedhub.core.VideoSource]、详情里有没有 [feedhub.core.FeedDetail.video]，由调用方判断。
 *
 * 这里只把"kind + 媒体第一项"这两件事实备齐。
 */
fun FeedItem.isVideoPost(): Boolean = RustPresent.isVideoPost(
    kindIsVideo = kind == ItemKind.VIDEO,
    firstMedia = when (val first = media.firstOrNull()) {
        null -> null
        is Media.Video -> RustPresent.MediaFirst.VIDEO
        is Media.Image -> RustPresent.MediaFirst.IMAGE
    },
)
