package feedhub.app.detail

import feedhub.present.RustPresentLinks

/**
 * 分享 = 标题 + 官方链接，链接里的跟踪参数要去掉。
 *
 * 规则全部在 Rust。
 */
fun cleanShareUrl(url: String): String =
    RustPresentLinks.cleanShareUrl(url)

/** 复制到剪贴板的分享文字：标题一行、链接一行；没有标题就只有链接。 */
fun shareText(title: String?, url: String): String =
    RustPresentLinks.shareText(title, url)
