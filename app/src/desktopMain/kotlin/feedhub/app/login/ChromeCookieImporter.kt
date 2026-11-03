package feedhub.app.login

import feedhub.chrome.RustChromeCookies
import feedhub.core.FeedCookieJar
import feedhub.core.Platform
import feedhub.core.plugin.CookieImportSpec
import feedhub.core.plugin.PlatformCatalog
import feedhub.plugins.InstalledPlugins
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import okhttp3.HttpUrl.Companion.toHttpUrl
import java.nio.file.Path

/**
 * 从本机 Chrome（Linux）导入 Cookie。
 *
 * 走的时候删掉）、用 `rusqlite` 只读打开、读版本 +
 * 六列、真有 `v11` 行时才跑 `kwallet-query` / `secret-tool`（超时 120 秒）、`v10` / `v11` 的
 * AES-128-CBC + PBKDF2-HMAC-SHA1、库版本 ≥ 24 的 `sha256(host_key)`、hostOnly / path / 过期、
 * 按域分组、要不要清域——**一个字节都没留在 Kotlin**。见 [RustChromeCookies]。
 *
 * 这个类只剩**三行胶水**：问插件描述"每个平台收哪些 host"、把结果写进 [cookies]（先 `clearDomain`
 * 再按域分组 `put`，顺序和原来一样），以及把"解不开"翻译成 `ImportResult`。
 * 只把数量交出去，Cookie 的值不在这一层出现（它们由 Rust 直接产成罐条目）。
 */
class ChromeCookieImporter(
    private val cookies: FeedCookieJar,
    private val profile: Path = RustChromeCookies.defaultProfile(),
    /** 每个平台收哪些 Cookie：插件描述里的 `login.chromeImport`（百度只取 `.baidu.com` 和 `tieba.baidu.com` 这类规则都在那里）。 */
    private val platforms: PlatformCatalog = InstalledPlugins.catalog,
) : CookieImporter {
    override val browserName = "Chrome"

    override fun supports(platform: Platform): Boolean = platforms[platform]?.login?.chromeImport != null

    override suspend fun import(platforms: Set<Platform>): Map<Platform, ImportResult> = withContext(Dispatchers.IO) {
        // 每个平台收哪些 host 来自插件描述（Rust 不认识插件）；一次问完，一次调用拿回全部平台。
        val specs = platforms.associateWith { p ->
            this@ChromeCookieImporter.platforms[p]?.login?.chromeImport
                ?: throw CookieImportException("${p.label}不支持从浏览器导入 Cookie")
        }
        val outcomes = try {
            // 解 + **写进罐子**都在 Rust：先 `clear_domain`、再按域分组写（一条都没收时不清）。
            facadeImport(profile, specs.mapValues { (_, spec) -> spec.toSpec() })
        } catch (e: java.io.IOException) {
            // 找不到库 / 库读不了 / 取不到钥匙环密码：界面层只认这一种异常（文案不带任何值）
            throw CookieImportException(e.message ?: e.toString())
        }
        platforms.associateWith { p ->
            val outcome = outcomes[p] ?: RustChromeCookies.Outcome(0, 0, null, emptyList())
            ImportResult(outcome.imported, outcome.failed)
        }
    }

    /**
     * 交给 Rust：它自己把解出来的条目写进 [cookies]（**先清域、再按域分组**，顺序和改动前一样）。
     *
     * 罐子必须是 Rust 那一份（[feedhub.cookies.RustCookieJar]）才有"直接写"的入口；别的实现
     * （测试里的假罐子）走 [RustChromeCookies.import] 拿条目、由这里照同样的顺序写。
     */
    private fun facadeImport(
        profile: Path,
        specs: Map<Platform, RustChromeCookies.Spec>,
    ): Map<Platform, RustChromeCookies.Outcome> {
        val rustJar = (cookies as? feedhub.cookies.RustCookieJar)?.ffi
        if (rustJar != null) {
            return RustChromeCookies.importInto(rustJar, profile, specs)
        }
        val outcomes = try {
            RustChromeCookies.import(profile, specs)
        } catch (e: java.io.IOException) {
            throw CookieImportException(e.message ?: e.toString())
        }
        for (outcome in outcomes.values) {
            outcome.clearDomain?.let { cookies.clearDomain(it) }
            for ((domain, list) in outcome.groups) {
                if (list.isEmpty()) continue
                cookies.put("https://$domain/".toHttpUrl(), list)
            }
        }
        return outcomes
    }

    private fun CookieImportSpec.toSpec() = RustChromeCookies.Spec(
        domain = domain,
        suffixes = suffixes,
        exactHosts = exactHosts,
    )
}
