package feedhub.app

import android.app.Activity
import android.content.ActivityNotFoundException
import android.content.Context
import android.content.Intent
import android.net.Uri
import android.os.Bundle

/**
 * 安卓的系统级动作（[SystemActions] 的实现），由 `androidGraph` 交给 [AppGraph.systemActions]。
 *
 * - **分享**：`Intent.ACTION_SEND`（`text/plain`）+ `Intent.createChooser`，内容照旧是"标题 + 去跟踪参数的官方链接"
 *   （[feedhub.app.detail.shareText]），所以微信 / QQ / 备忘录收到的和以前复制到剪贴板的一模一样。
 * - **打开网页**：Custom Tabs。工程里没有 `androidx.browser` 依赖，
 *   所以按 CustomTabsIntent 打的那组 extra 自己拼一个 `ACTION_VIEW` —— 不认这些 extra 的浏览器就是一个普通标签页。
 * - **深链**：`ACTION_VIEW` + `bilibili://…` 这类地址，交给官方 App。
 *
 * 两种"打不开"都返回 false 让调用方退化（浏览器 → 复制链接），**不弹自己的错误**：
 * 没有浏览器的设备极少，没有官方 App 的设备很常见（`ActivityNotFoundException`）。
 *
 * @param context 拿得到 Activity 就用它（`startActivity` 会进同一个任务栈）；只有应用上下文时补
 *   `FLAG_ACTIVITY_NEW_TASK`（预览 / 测试的组装）。
 */
class AndroidSystemActions(context: Context) : SystemActions {
    private val appContext: Context = context.applicationContext

    @Volatile
    private var activity: Activity? = context as? Activity

    override val available: Boolean get() = true

    fun attach(activity: Activity) {
        this.activity = activity
    }

    fun detach(activity: Activity) {
        if (this.activity === activity) {
            this.activity = null
        }
    }

    override fun share(text: String): Boolean {
        val send = Intent(Intent.ACTION_SEND).setType("text/plain").putExtra(Intent.EXTRA_TEXT, text)
        // chooser 自己处理"哪个 App 能分享"，永远有得选（系统面板里至少有"复制"）
        return launch(Intent.createChooser(send, "分享"))
    }

    // 只放行网页地址：正文链接是第三方写的，`file://` / `content://` / `intent:` 交给系统会崩或越权；
    // 别的 scheme 返回 false，调用方走"复制链接"。深链走 [openApp]（入参来自我们自己的平台描述）。
    override fun openWeb(url: String): Boolean = isWebUrl(url) && launch(
        Intent(Intent.ACTION_VIEW, Uri.parse(url)).apply {
            // CustomTabsIntent 打的就是这几个 extra（androidx.browser 的常量值）：有 Chrome / 支持
            // Custom Tabs 的浏览器会开一个带返回栈的轻量页，不支持的当普通标签页。
            // SESSION 的值是 null（我们不用 postMessage 回传），走 Bundle 是因为 Intent.putExtra(String, IBinder)
            // 在 Kotlin 里传 null 会被重载解析绕晕（Bundle.putBinder 只有一个签名）。
            putExtras(Bundle().apply { putBinder(EXTRA_CUSTOM_TABS_SESSION, null) })
            putExtra(EXTRA_CUSTOM_TABS_ENABLE_INSTANT_APPS, true)
        },
    )

    override fun openApp(deepLink: String): Boolean =
        launch(Intent(Intent.ACTION_VIEW, Uri.parse(deepLink)))

    /** 真的启动出去了返回 true；没有 App 能接这个 Intent（或 ROM 拒绝）返回 false。 */
    private fun launch(intent: Intent): Boolean = try {
        val act = activity?.takeIf { !it.isFinishing && !it.isDestroyed }
        if (act != null) {
            act.startActivity(intent)
        } else {
            intent.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
            appContext.startActivity(intent)
        }
        true
    } catch (e: ActivityNotFoundException) {
        false
    } catch (e: RuntimeException) {
        // SecurityException / FileUriExposedException 之类：ROM 拒绝或 URI 不能外露，同样当"没打开"
        false
    }

    private companion object {
        fun isWebUrl(url: String): Boolean = Uri.parse(url.trim()).scheme?.lowercase() in setOf("http", "https")

        /** `CustomTabsIntent.EXTRA_SESSION`。 */
        const val EXTRA_CUSTOM_TABS_SESSION = "android.support.customtabs.extra.SESSION"

        /** `CustomTabsIntent.EXTRA_ENABLE_INSTANT_APPS`。 */
        const val EXTRA_CUSTOM_TABS_ENABLE_INSTANT_APPS = "android.support.customtabs.extra.EXTRA_ENABLE_INSTANT_APPS"
    }
}
