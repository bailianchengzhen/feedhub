package feedhub.app.detail

import feedhub.app.SystemActions
import feedhub.core.FeedItem
import feedhub.core.UserRef
import feedhub.core.plugin.PlatformCatalog
import feedhub.plugins.InstalledPlugins
import feedhub.present.RustPresentLinks
import uniffi.feedhub_ffi.FfiExternalOpenResult

/**
 * "在外面打开"：官方 App 优先（插件描述里的深链），没有深链或者 App 没装就退化成网页，
 * 网页也打不开就复制链接——**每一步都退得下去**，不会点了没反应。
 *
 * 深链格式不在这里：各平台写在插件描述 `PlatformPlugin.appLinks`（`bilibili://video/{id}` 这种模板），
 * 这个文件只负责"选哪个地址、按什么顺序打开"，所以平台再多也不用改这里（`PluginBoundaryTest` 也不允许按平台 id 分支）。
 *
 * 决策逻辑与回退状态机在 Rust。
 *
 * @param deepLink 官方 App 的深链；null = 这个平台 / 这类内容没有拿得准的深链（只给网页）。
 * @param webUrl 网页地址（已去掉跟踪参数，见 [cleanShareUrl]）；null = 连网页都没有。
 * @param label 菜单 / 按钮上写什么："在 B站 打开" / "用浏览器打开"。
 */
data class ExternalOpen(
    val deepLink: String?,
    val webUrl: String?,
    val label: String,
)

/** [performExternalOpen] 的结果：界面上只在"复制了 / 打不开"时说一句，打开成功不用提示。 */
enum class ExternalOpenResult {
    /** 交给官方 App 了。 */
    OPENED_APP,

    /** 交给浏览器了。 */
    OPENED_WEB,

    /** 都打不开，已经复制链接到剪贴板。 */
    COPIED,

    /** 连网页地址都没有，什么也没做成。 */
    FAILED;

    internal companion object {
        fun fromFfi(result: FfiExternalOpenResult): ExternalOpenResult = when (result) {
            FfiExternalOpenResult.OPENED_APP -> OPENED_APP
            FfiExternalOpenResult.OPENED_WEB -> OPENED_WEB
            FfiExternalOpenResult.COPIED -> COPIED
            FfiExternalOpenResult.FAILED -> FAILED
        }
    }
}

/** 这条内容"在官方 App / 浏览器打开"的那一项；没有可打开的地址返回 null（界面就不显示这一项）。 */
fun externalOpenOfItem(item: FeedItem, platforms: PlatformCatalog = InstalledPlugins.catalog): ExternalOpen? =
    externalOpen(
        deepLink = platforms[item.platform]?.appLinks?.detailLink(item),
        webUrl = item.url,
        platformLabel = item.platform.label,
    )

/** 这个作者主页"在官方 App / 浏览器打开"的那一项；没有可打开的地址返回 null。 */
fun externalOpenOfAuthor(author: UserRef, platforms: PlatformCatalog = InstalledPlugins.catalog): ExternalOpen? =
    externalOpen(
        deepLink = platforms[author.platform]?.appLinks?.userLink(author),
        webUrl = author.url,
        platformLabel = author.platform.label,
    )

internal fun externalOpen(deepLink: String?, webUrl: String?, platformLabel: String): ExternalOpen? =
    RustPresentLinks.planExternalOpen(deepLink, webUrl, platformLabel)?.let {
        ExternalOpen(
            deepLink = it.deepLink,
            webUrl = it.webUrl,
            label = it.label,
        )
    }

/**
 * 真去打开：官方 App → 浏览器 → 复制链接。**桌面**（[SystemActions.None]）两步都返回 false，
 * 所以这里自动退化成复制，和以前一样，不用在调用点判断平台。
 *
 * @param copy 复制到剪贴板（桌面 / 都打不开时用）。
 */
fun performExternalOpen(
    actions: SystemActions,
    open: ExternalOpen,
    copy: (String) -> Unit,
): ExternalOpenResult = ExternalOpenResult.fromFfi(
    RustPresentLinks.performExternalOpen(
        deepLink = open.deepLink,
        webUrl = open.webUrl,
        openApp = { actions.openApp(it) },
        openWeb = { actions.openWeb(it) },
        copy = copy,
    )
)
