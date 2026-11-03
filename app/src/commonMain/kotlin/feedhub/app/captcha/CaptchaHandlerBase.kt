package feedhub.app.captcha

import feedhub.captcha.RustCaptcha
import feedhub.core.CaptchaChallenge
import feedhub.core.CaptchaHandler
import feedhub.core.Platform
import feedhub.core.RequestScope
import feedhub.core.RiskControlException
import feedhub.core.plugin.CaptchaPage
import feedhub.plugins.InstalledPlugins
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.NonCancellable
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.withContext
import kotlinx.coroutines.withTimeoutOrNull
import uniffi.feedhub_ffi.FfiCaptchaActivePage
import uniffi.feedhub_ffi.FfiCaptchaSubmitResult
import kotlin.coroutines.CoroutineContext

/**
 * 内嵌浏览器要到验证页、或者页面把 proof 回传回来时问它。
 */
interface CaptchaPageHost {
    /** [url] 正好是当前那张验证页就返回 HTML；**别的一律 null**。 */
    fun pageFor(url: String): String?

    /** 页面把 proof 回传到 [url]：地址 / token / 格式都对、并且收下了才返回 true。 */
    fun submit(url: String): Boolean
}

/**
 * 桌面 / 安卓共用的验证码流程：**排队、取消、超时、提示状态、proof 校验**。
 * 核心规则与校验全部由 Rust（`RustCaptcha`）执行。
 */
abstract class CaptchaHandlerBase(
    val prompt: CaptchaPromptState,
    protected val uiContext: CoroutineContext = Dispatchers.Main,
    protected val pages: (Platform) -> CaptchaPage? = { InstalledPlugins.catalog[it]?.captchaPage },
    private val timeoutMs: Long = DEFAULT_TIMEOUT_MS,
) : CaptchaHandler {

    private val active = MutableStateFlow<Active?>(null)

    /** 正在弹的那道题：同一道题（平台 + `captchaId` + `field`）才合并等同一个结果。 */
    private class Active(val key: Triple<String, String, String>, val deferred: CompletableDeferred<String?>)
    private val mounted = MutableStateFlow<ActivePage?>(null)

    private data class ActivePage(
        val scheme: String,
        val host: String,
        val port: Int,
        val path: String,
        val token: String,
        val html: String,
        val deferred: CompletableDeferred<String?>,
    ) {
        val ffi: FfiCaptchaActivePage = FfiCaptchaActivePage(
            scheme = scheme,
            host = host,
            port = port,
            path = path,
            token = token,
        )
    }

    data class MountedPage(val url: String, val html: String, val close: () -> Unit = {})

    protected val currentUrl: String? get() = mounted.value?.let { RustCaptcha.currentUrl(it.ffi) }

    final override suspend fun solve(challenge: CaptchaChallenge): String? {
        // 对外接口的调用（协程上下文里有它的 RequestScope）不弹验证：验证只能用户本人在 App 里做，
        // 接口这一侧回 `captcha_required`，不能在用户界面上弹东西、也不把接口线程挂几分钟
        if (currentCoroutineContext()[RequestScope] != null) {
            throw RiskControlException(
                challenge.platform,
                CAPTCHA_CODE,
                "${challenge.platform.label}要求人机验证（${challenge.message}）：只能在 App 里由用户本人完成",
            )
        }
        val key = Triple(challenge.platform.key, challenge.captchaId, challenge.field)
        val mine: Active
        while (true) {
            val existing = active.value
            if (existing != null) {
                if (existing.key == key) return existing.deferred.await()
                // 不同的题：等前一道**整个收尾完**（页面卸掉、覆盖层收起）再弹，拿前一道的 proof 去答这一道必然失败；
                // 只等它的结果不够——它的 finally 会把后弹的这一道的页面 / 覆盖层也一起收掉
                active.first { it !== existing }
                continue
            }
            val fresh = Active(key, CompletableDeferred())
            if (active.compareAndSet(null, fresh)) {
                mine = fresh
                break
            }
        }
        val deferred = mine.deferred
        try {
            return runPage(challenge, deferred)
        } finally {
            mounted.value = null
            // 先收覆盖层、再让出 `active`：排着的另一道题这时才会挂自己的页面（见上面的 `active.first`）
            withContext(uiContext + NonCancellable) { prompt.hide() }
            active.compareAndSet(mine, null)
            deferred.complete(null)
        }
    }

    final override fun cancel() {
        active.value?.deferred?.complete(null)
    }

    private suspend fun runPage(challenge: CaptchaChallenge, deferred: CompletableDeferred<String?>): String? {
        val page = pages(challenge.platform) ?: return null
        if (refuseReason() != null) return null

        val token = randomToken()
        val submitUrl = RustCaptcha.submitUrl(token)
        val opened = mountPage(challenge, page, submitUrl) ?: return null
        val parsed = RustCaptcha.parseHttpUrl(opened.url)
        if (parsed == null) {
            runCatching { opened.close() }
            return null
        }
        mounted.value = ActivePage(
            scheme = parsed.scheme,
            host = parsed.host,
            port = parsed.port,
            path = parsed.encodedPath,
            token = token,
            html = opened.html,
            deferred = deferred,
        )
        try {
            withContext(uiContext) {
                prompt.show(
                    platformLabel = challenge.platform.label,
                    message = RustCaptcha.promptMessage(challenge.platform.label, challenge.message),
                    url = opened.url,
                    status = "请在下面的验证码里完成验证…",
                )
            }
            val proof = withTimeoutOrNull(timeoutMs) { deferred.await() }
            if (proof == null) {
                withContext(uiContext) { prompt.updateStatus("没有等到验证结果。下次打开评论会再试。") }
            }
            return proof
        } finally {
            mounted.value = null
            runCatching { opened.close() }
        }
    }

    protected abstract suspend fun mountPage(
        challenge: CaptchaChallenge,
        page: CaptchaPage,
        submitUrl: String,
    ): MountedPage?

    protected open fun refuseReason(): String? = null

    // ---------- 给 CaptchaPageHost 用 ----------

    protected fun currentPageHtml(url: String): String? {
        val current = mounted.value ?: return null
        return if (RustCaptcha.matchesActivePage(current.ffi, url)) current.html else null
    }

    protected fun acceptSubmit(url: String): Boolean {
        val accepted = checkSubmit(url) as? SubmitResult.Accepted ?: return false
        deliverProof(accepted.proof)
        return true
    }

    protected sealed interface SubmitResult {
        data class Accepted(val proof: String) : SubmitResult
        data object WrongToken : SubmitResult
        data object BadProof : SubmitResult
        data object NoPage : SubmitResult
    }

    protected fun checkSubmit(url: String): SubmitResult =
        when (val res = RustCaptcha.checkSubmit(mounted.value?.ffi, url)) {
            is FfiCaptchaSubmitResult.Accepted -> SubmitResult.Accepted(res.proof)
            FfiCaptchaSubmitResult.WrongToken -> SubmitResult.WrongToken
            FfiCaptchaSubmitResult.BadProof -> SubmitResult.BadProof
            FfiCaptchaSubmitResult.NoPage -> SubmitResult.NoPage
        }

    protected fun completeProof(proof: String) {
        mounted.value?.deferred?.complete(proof)
    }

    protected open fun deliverProof(proof: String) {
        completeProof(proof)
    }

    companion object {
        const val DEFAULT_TIMEOUT_MS = 5 * 60 * 1000L

        /** "要人机验证"的风控码（和 `CoolapkCaptcha` 的一致；对外接口把它映射成 `captcha_required`）。 */
        const val CAPTCHA_CODE = "captcha"
        const val PROOF_PATH = "/proof"

        internal fun randomToken(): String = RustCaptcha.generateRandomToken()

        internal fun validProof(proof: String): Boolean = RustCaptcha.validProof(proof)
    }
}
