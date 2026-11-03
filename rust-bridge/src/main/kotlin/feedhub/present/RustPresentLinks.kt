package feedhub.present

import feedhub.rust.RustNative
import uniffi.feedhub_ffi.FfiExternalOpen
import uniffi.feedhub_ffi.FfiExternalOpenResult
import uniffi.feedhub_ffi.FfiExternalOpenStage
import uniffi.feedhub_ffi.FfiExternalOpenStep
import uniffi.feedhub_ffi.presentCleanShareUrl
import uniffi.feedhub_ffi.presentNextExternalOpenStep
import uniffi.feedhub_ffi.presentPlanExternalOpen
import uniffi.feedhub_ffi.presentShareText

/**
 * **链接清洗、分享文本拼装与外部打开决策**。
 *
 * 原实现是 `app/.../detail/Share.kt` 和 `app/.../detail/OpenLinks.kt`：
 * - 分享链接各平台跟踪参数清洗；
 * - 复制分享文本拼装（两步空白字符处理：正则 ASCII 空白折叠 + Kotlin 全角/NBSP 两端 trim）；
 * - 外部打开选择（深链优先、网页次之、文案生成）；
 * - 外部打开回退决策（先试 App → 失败试 Web → 失败复制链接 → 连 Web 都没有返回失败）。
 *
 * 逻辑全部在 `rust/crates/feedhub-core/src/present/links.rs`，这里是其 Kotlin 门面。
 */
object RustPresentLinks {

    /** 清洗分享链接里的跟踪参数（原 `cleanShareUrl`）。 */
    fun cleanShareUrl(url: String): String = native {
        presentCleanShareUrl(url)
    }

    /** 复制到剪贴板的分享文字：标题一行、链接一行（原 `shareText`）。 */
    fun shareText(title: String?, url: String): String = native {
        presentShareText(title, url)
    }

    /** 决策外部打开的候选链接与按钮标签（原 `externalOpen`）。 */
    fun planExternalOpen(
        deepLink: String?,
        webUrl: String?,
        platformLabel: String,
    ): FfiExternalOpen? = native {
        presentPlanExternalOpen(deepLink, webUrl, platformLabel)
    }

    /**
     * 驱动外部打开回退状态机（原 `performExternalOpen` 决策部分）。
     *
     * 真正的系统动作调用留在宿主回调中。
     */
    fun performExternalOpen(
        deepLink: String?,
        webUrl: String?,
        openApp: (String) -> Boolean,
        openWeb: (String) -> Boolean,
        copy: (String) -> Unit,
    ): FfiExternalOpenResult = native {
        var stage = FfiExternalOpenStage.INITIAL
        while (true) {
            when (val step = presentNextExternalOpenStep(deepLink, webUrl, stage)) {
                is FfiExternalOpenStep.OpenApp -> {
                    if (openApp(step.target)) return@native FfiExternalOpenResult.OPENED_APP
                    stage = FfiExternalOpenStage.APP_FAILED
                }
                is FfiExternalOpenStep.OpenWeb -> {
                    if (openWeb(step.target)) return@native FfiExternalOpenResult.OPENED_WEB
                    stage = FfiExternalOpenStage.WEB_FAILED
                }
                is FfiExternalOpenStep.CopyWeb -> {
                    copy(step.target)
                    return@native FfiExternalOpenResult.COPIED
                }
                is FfiExternalOpenStep.Completed -> {
                    return@native step.result
                }
            }
        }
        @Suppress("UNREACHABLE_CODE")
        FfiExternalOpenResult.FAILED
    }

    private inline fun <T> native(block: () -> T): T = try {
        RustNative.ensureLoaded()
        block()
    } catch (e: Exception) {
        throw IllegalStateException("链接与外部打开规则在 Rust 里，但 .so 没加载起来：${e.message}", e)
    }
}
