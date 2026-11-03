package feedhub.captcha

import feedhub.rust.RustNative
import uniffi.feedhub_ffi.FfiCaptchaActivePage
import uniffi.feedhub_ffi.FfiCaptchaCommand
import uniffi.feedhub_ffi.FfiCaptchaEvent
import uniffi.feedhub_ffi.FfiCaptchaHttpUrl
import uniffi.feedhub_ffi.FfiCaptchaParsedUrl
import uniffi.feedhub_ffi.FfiCaptchaRectPlan
import uniffi.feedhub_ffi.FfiCaptchaSubmitResult
import uniffi.feedhub_ffi.ffiCaptchaBuildLauncherCommand
import uniffi.feedhub_ffi.ffiCaptchaBuildLauncherEnv
import uniffi.feedhub_ffi.ffiCaptchaCalculateLogicalSize
import uniffi.feedhub_ffi.ffiCaptchaCheckSubmit
import uniffi.feedhub_ffi.ffiCaptchaCurrentUrl
import uniffi.feedhub_ffi.ffiCaptchaDecodeCommand
import uniffi.feedhub_ffi.ffiCaptchaDecodeEventHeader
import uniffi.feedhub_ffi.ffiCaptchaEncodeCommand
import uniffi.feedhub_ffi.ffiCaptchaEncodeEventHeader
import uniffi.feedhub_ffi.ffiCaptchaGenerateRandomToken
import uniffi.feedhub_ffi.ffiCaptchaIsMainFrameAllowed
import uniffi.feedhub_ffi.ffiCaptchaIsSubFrameAllowed
import uniffi.feedhub_ffi.ffiCaptchaMatchesActivePage
import uniffi.feedhub_ffi.ffiCaptchaParseHttpUrl
import uniffi.feedhub_ffi.ffiCaptchaParseUrl
import uniffi.feedhub_ffi.ffiCaptchaPlanRect
import uniffi.feedhub_ffi.ffiCaptchaPortOf
import uniffi.feedhub_ffi.ffiCaptchaPromptMessage
import uniffi.feedhub_ffi.ffiCaptchaRedactUrl
import uniffi.feedhub_ffi.ffiCaptchaSubmitUrl
import uniffi.feedhub_ffi.ffiCaptchaValidProof

/**
 * 验证码流程的 Rust 核心门面。
 *
 * 搬迁项包括：
 * - 导航白名单策略（`CaptchaNavigationPolicy`）；
 * - 父子进程二进制帧编解码（`HelperProtocol`）；
 * - 挂载匹配、proof 形状校验与回传校验四态判定（`CaptchaHandlerBase`）；
 * - 子进程启动参数拼装与尺寸换算（`CaptchaHelperLauncher` / `CaptchaHelperSession`）。
 *
 * 宿主只留：CEF 初始化、子进程生命周期控制与收割、管道读写、Compose 与协程调度。
 */
object RustCaptcha {

    fun parseUrl(raw: String): FfiCaptchaParsedUrl? = native {
        ffiCaptchaParseUrl(raw)
    }

    fun portOf(url: String): Int = native {
        ffiCaptchaPortOf(url)
    }

    fun isMainFrameAllowed(port: Int, url: String): Boolean = native {
        ffiCaptchaIsMainFrameAllowed(port, url)
    }

    fun isSubFrameAllowed(url: String): Boolean = native {
        ffiCaptchaIsSubFrameAllowed(url)
    }

    fun validProof(proof: String): Boolean = native {
        ffiCaptchaValidProof(proof)
    }

    fun generateRandomToken(): String = native {
        ffiCaptchaGenerateRandomToken()
    }

    fun submitUrl(token: String): String = native {
        ffiCaptchaSubmitUrl(token)
    }

    fun promptMessage(platformLabel: String, challengeMessage: String): String = native {
        ffiCaptchaPromptMessage(platformLabel, challengeMessage)
    }

    fun parseHttpUrl(url: String): FfiCaptchaHttpUrl? = native {
        ffiCaptchaParseHttpUrl(url)
    }

    fun currentUrl(page: FfiCaptchaActivePage): String = native {
        ffiCaptchaCurrentUrl(page)
    }

    fun matchesActivePage(page: FfiCaptchaActivePage?, url: String): Boolean = native {
        ffiCaptchaMatchesActivePage(page, url)
    }

    fun checkSubmit(page: FfiCaptchaActivePage?, url: String): FfiCaptchaSubmitResult = native {
        ffiCaptchaCheckSubmit(page, url)
    }

    fun encodeCommand(cmd: FfiCaptchaCommand): ByteArray = native {
        ffiCaptchaEncodeCommand(cmd)
    }

    fun decodeCommand(bytes: ByteArray): FfiCaptchaCommand? = native {
        ffiCaptchaDecodeCommand(bytes)
    }

    fun encodeEventHeader(event: FfiCaptchaEvent): ByteArray = native {
        ffiCaptchaEncodeEventHeader(event)
    }

    fun encodeHello(version: Int, pid: Long): ByteArray =
        encodeEventHeader(FfiCaptchaEvent.Hello(version, pid))

    fun encodeStatus(text: String): ByteArray =
        encodeEventHeader(FfiCaptchaEvent.Status(text))

    fun encodeCursor(cursorType: Int): ByteArray =
        encodeEventHeader(FfiCaptchaEvent.Cursor(cursorType))

    fun encodeLoaded(httpStatus: Int): ByteArray =
        encodeEventHeader(FfiCaptchaEvent.Loaded(httpStatus))

    fun encodeFailed(message: String): ByteArray =
        encodeEventHeader(FfiCaptchaEvent.Failed(message))

    fun encodeBye(): ByteArray =
        encodeEventHeader(FfiCaptchaEvent.Bye)

    fun encodeFrameHeader(
        width: Int,
        height: Int,
        rects: Int,
        paintStamp: Long,
        inputStamp: Long,
    ): ByteArray =
        encodeEventHeader(FfiCaptchaEvent.Frame(width, height, rects, paintStamp, inputStamp))

    fun decodeEventHeader(bytes: ByteArray): FfiCaptchaEvent? = native {
        ffiCaptchaDecodeEventHeader(bytes)
    }

    fun planRect(
        frameW: Int,
        frameH: Int,
        x: Int,
        y: Int,
        w: Int,
        h: Int,
    ): FfiCaptchaRectPlan = native {
        ffiCaptchaPlanRect(frameW, frameH, x, y, w, h)
    }

    fun buildLauncherCommand(
        javaBin: String?,
        canExecuteJava: Boolean,
        classPath: String,
        mainClass: String,
        appPath: String?,
        canExecuteApp: Boolean,
    ): List<String> = native {
        ffiCaptchaBuildLauncherCommand(
            javaBin = javaBin,
            canExecuteJava = canExecuteJava,
            classPath = classPath,
            mainClass = mainClass,
            appPath = appPath,
            canExecuteApp = canExecuteApp,
        )
    }

    fun buildLauncherEnv(
        appPath: String?,
        archivePath: String?,
    ): Map<String, String?> = native {
        ffiCaptchaBuildLauncherEnv(appPath, archivePath).associate { it.key to it.value }
    }

    fun calculateLogicalSize(
        widthPx: Int,
        heightPx: Int,
        density: Float,
    ): Pair<Int, Int> = native {
        val size = ffiCaptchaCalculateLogicalSize(widthPx, heightPx, density)
        size.width to size.height
    }

    fun redactUrl(url: String?): String = native {
        ffiCaptchaRedactUrl(url)
    }

    private inline fun <T> native(block: () -> T): T {
        try {
            RustNative.ensureLoaded()
        } catch (e: Exception) {
            throw IllegalStateException("验证码规则在 Rust 里，但 .so 没加载起来：${e.message}", e)
        }
        return try {
            block()
        } catch (e: uniffi.feedhub_ffi.FfiCaptchaException) {
            throw java.io.IOException(e.message, e)
        }
    }
}
