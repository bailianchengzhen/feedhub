package feedhub.cli

import feedhub.core.CaptchaChallenge
import feedhub.core.CaptchaHandler
import feedhub.core.Platform
import feedhub.core.plugin.CaptchaPage
import feedhub.plugins.InstalledPlugins
import java.nio.file.Files
import java.nio.file.Path

/**
 * cli 的验证码：把这个平台的验证页（插件描述的 `captchaPage`，现在只有酷安）写到临时文件，让用户用浏览器打开并完成验证，
 * 再把页面显示的 `NEC:...` 整行粘贴回终端。
 *
 * 为什么不用本地 HTTP 服务自动回传：cli 可能在 ssh / 没有浏览器的环境里跑，粘贴一条文本更可靠；
 * App 桌面端才用 `DesktopCaptchaHandler` 的自动回传。两条路都**只把用户拿到的 validate 交给 Source**，
 * 不解验证码。
 */
class ConsoleCaptchaHandler(
    private val out: (String) -> Unit = { println(it) },
    private val input: () -> String? = { readLine() },
    /** 各平台的验证页（插件描述的 `captchaPage`，现在只有酷安）。 */
    private val pages: (Platform) -> CaptchaPage? = { InstalledPlugins.catalog[it]?.captchaPage },
) : CaptchaHandler {
    override suspend fun solve(challenge: CaptchaChallenge): String? {
        val html = pages(challenge.platform)?.html(challenge.captchaId, null) ?: run {
            out("${challenge.platform.label}要人机验证（${challenge.message}），但这个插件没带验证页，放弃。")
            return null
        }
        val page = writePage(html)
        out("${challenge.platform.label}评论需要人机验证（${challenge.message}）。")
        out("验证页已写到：${page.toAbsolutePath()}")
        out("用浏览器打开它，完成验证，把页面里显示的一整行（NEC:...）粘贴到这里；直接回车放弃。")
        out("验证结果：")
        runCatching { ProcessBuilder("xdg-open", page.toAbsolutePath().toString()).start() }
        val line = input()?.trim().orEmpty()
        Files.deleteIfExists(page)
        if (line.isEmpty()) {
            out("已放弃这次验证。")
            return null
        }
        // 用户可能把整行连同提示一起复制；只认 NEC: 开头的部分
        val proof = line.substringAfter("NEC:", "").let { if (it.isEmpty()) null else "NEC:$it" }
        if (proof == null || proof.length < 14) {
            out("这不像验证结果（应以 NEC: 开头），已放弃。")
            return null
        }
        return proof
    }

    private fun writePage(html: String): Path {
        val page = Files.createTempFile("feedhub-captcha-", ".html")
        Files.writeString(page, html)
        page.toFile().deleteOnExit()
        return page
    }
}
