package feedhub.chrome

import feedhub.rust.RustNative
import uniffi.feedhub_ffi.FfiCookieImportGroup
import uniffi.feedhub_ffi.FfiCookieImportSpec
import uniffi.feedhub_ffi.FfiPlatformImportSpec
import uniffi.feedhub_ffi.FfiPlatformImportOutcome
import uniffi.feedhub_ffi.chromeCookieDefaultProfile
import uniffi.feedhub_ffi.chromeCookieImport
import uniffi.feedhub_ffi.chromeCookieImportFile
import uniffi.feedhub_ffi.chromeCookieImportFileWithCommands
import uniffi.feedhub_ffi.cookieJarImportChrome
import uniffi.feedhub_ffi.cookieJarImportChromeWithCommands

/**
 * **从 Chrome 导入 Cookie 的 Kotlin 门面**。
 *
 * 走的时候整棵删掉）、用 **`rusqlite`**
 *   只读打开、读 `meta.version` 和 `cookies` 表的六列、真有 `v11` 行时才跑 `kwallet-query` /
 *   `secret-tool`（`std::process`，超时 120 秒、读 stdout、丢 stderr）；
 * - `chrome_cookies.rs`：每个平台收哪些行、`expires_utc` 换算、`v10` / `v11` 前缀、AES-128-CBC +
 *   PBKDF2-HMAC-SHA1、库版本 ≥ 24 的
 *   `sha256(host_key)`、hostOnly / path / 过期、按域分组、要不要清域；
 * - 写进罐子（先 `clear_domain` 再按域分组 `save_from_response`）也在 Rust。
 *
 * 宿主只剩"用户点了导入"：这个 object 传一个 profile 路径和平台清单，拿回**数量**。
 * **Cookie 的值 / 密码 / key 都不过在边界上**（过边界的只有最终要写进罐子的条目——它们本来就是罐子的内容）。
 */
object RustChromeCookies {
    /** 导入范围（插件描述的 `login.chromeImport`）。 */
    data class Spec(val domain: String, val suffixes: List<String>, val exactHosts: Set<String>)

    /** 一次导入的结论：**只有数量**，从来没有值。 */
    data class Outcome(
        val imported: Int,
        val failed: Int,
        /** 先清哪个域；null = 一条都没收（**不清**，和 Kotlin 原来那句 `if (imported.isNotEmpty())` 一致）。 */
        val clearDomain: String?,
        /** 写进罐子的条目，**已经按域分好组**（组的先后 = 域第一次出现的先后）。 */
        val groups: List<Pair<String, List<okhttp3.Cookie>>>,
    )

    /** 一条从 Chrome 库里读出来的行。 */
    data class Row(
        val hostKey: String,
        val name: String,
        val path: String,
        val expiresUtc: Long,
        val isSecure: Boolean,
        val isHttpOnly: Boolean,
        val encryptedValue: ByteArray,
    ) {
        // ByteArray 的 equals / hashCode 要自己写（data class 给的是引用比较）
        override fun equals(other: Any?): Boolean = other is Row &&
            hostKey == other.hostKey && name == other.name && path == other.path &&
            expiresUtc == other.expiresUtc && isSecure == other.isSecure &&
            isHttpOnly == other.isHttpOnly && encryptedValue.contentEquals(other.encryptedValue)

        override fun hashCode(): Int = encryptedValue.contentHashCode() * 31 + name.hashCode()
    }

    /**
     * **纯函数入口**：直接解一批已经读出来的行。
     *
     * 生产路径不走这里（它走 [import] / [importInto]：库是 Rust 自己读的）。留着它是为了
     * `ChromeCookieImportParityTest`——那份测试拿 JDK 的 `javax.crypto` 造密文，逐字比 Rust 的解密。
     */
    fun import(
        dbVersion: Long,
        passwordV10: String,
        passwordV11: String,
        rows: List<Row>,
        spec: Spec,
        nowMs: Long,
    ): Outcome? {
        RustNative.ensureLoaded()
        val ffi = chromeCookieImport(
            dbVersion = dbVersion,
            passwordV10 = passwordV10,
            passwordV11 = passwordV11,
            rows = rows.map {
                uniffi.feedhub_ffi.FfiChromeCookieRow(
                    hostKey = it.hostKey,
                    name = it.name,
                    path = it.path,
                    expiresUtc = it.expiresUtc,
                    isSecure = it.isSecure,
                    isHttponly = it.isHttpOnly,
                    encryptedValue = it.encryptedValue,
                )
            },
            spec = FfiCookieImportSpec(
                domain = spec.domain,
                suffixes = spec.suffixes,
                exactHosts = spec.exactHosts.toList(),
            ),
            nowMs = nowMs,
        ) ?: return null
        return Outcome(
            imported = ffi.imported.toInt(),
            failed = ffi.failed.toInt(),
            clearDomain = ffi.clearDomain,
            groups = ffi.groups.map { g: FfiCookieImportGroup -> g.domain to g.cookies.map(::toCookie) },
        )
    }

    /**
     * 从一个 Chrome profile 目录导入这些平台（**不去写罐子**；纯函数版本的收条版本）。
     *
     * @param profile Chrome profile 目录（`<profile>/Cookies` 是那个库）。
     * @param platforms 平台 key → 收哪些 host。
     * @param nowMs 注入的时钟（判过期；测试里固定）。
     * @return 每个平台一条（顺序 = 传进来的顺序）。
     * 找不到库 / 库读不了 / 取不到密码都抛 [java.io.IOException]（文案和改动前一样，不带任何值）；
     * 界面那一层（`ChromeCookieImporter`）把它翻成 `CookieImportException`。
     */
    fun import(
        profile: java.nio.file.Path,
        platforms: Map<feedhub.core.Platform, Spec>,
        nowMs: Long = System.currentTimeMillis(),
    ): Map<feedhub.core.Platform, Outcome> {
        RustNative.ensureLoaded()
        val keys = platforms.keys.toList()
        val ffi = try {
            chromeCookieImportFile(
                profile = profile.toString(),
                platforms = keys.map { key ->
                    val spec = platforms.getValue(key)
                    FfiPlatformImportSpec(
                        platform = key.key,
                        spec = FfiCookieImportSpec(
                            domain = spec.domain,
                            suffixes = spec.suffixes,
                            exactHosts = spec.exactHosts.toList(),
                        ),
                    )
                },
                nowMs = nowMs,
            )
        } catch (e: uniffi.feedhub_ffi.FfiCookieException) {
            throw java.io.IOException(describe(e), e)
        }
        return toOutcomes(keys, ffi)
    }

    /**
     * 同 [import]，但**结果直接写进罐子**（先 `clear_domain`、再按域分组 `save_from_response`，
     * 顺序和改动前一样）——这两步也在 Rust（`cookie_jar_import_chrome`）。
     *
     * 宿主不碰解出来的 Cookie，只把数量翻成 `ImportResult`。
     */
    fun importInto(
        jar: uniffi.feedhub_ffi.CookieJar,
        profile: java.nio.file.Path,
        platforms: Map<feedhub.core.Platform, Spec>,
        nowMs: Long = System.currentTimeMillis(),
    ): Map<feedhub.core.Platform, Outcome> {
        RustNative.ensureLoaded()
        val keys = platforms.keys.toList()
        val ffi = try {
            cookieJarImportChrome(
                jar = jar,
                profile = profile.toString(),
                platforms = keys.map { key ->
                    val spec = platforms.getValue(key)
                    FfiPlatformImportSpec(
                        platform = key.key,
                        spec = FfiCookieImportSpec(
                            domain = spec.domain,
                            suffixes = spec.suffixes,
                            exactHosts = spec.exactHosts.toList(),
                        ),
                    )
                },
                nowMs = nowMs,
            )
        } catch (e: uniffi.feedhub_ffi.FfiCookieException) {
            throw java.io.IOException(describe(e), e)
        }
        return toOutcomes(keys, ffi)
    }

    /**
     * 测试 / 排障用的那一份：换一组"取密码"的命令（不碰真的 KWallet / 钥匙环）。
     *
     * `passwordCommands` 为空 = 不要密码（遇到 `v11` 行就报"取不到密钥"）。
     */
    fun importForTest(
        profile: java.nio.file.Path,
        platforms: Map<feedhub.core.Platform, Spec>,
        passwordCommands: List<List<String>>,
        passwordTimeoutMs: Long,
        nowMs: Long,
    ): Map<feedhub.core.Platform, Outcome> {
        RustNative.ensureLoaded()
        val keys = platforms.keys.toList()
        val ffi = try {
            chromeCookieImportFileWithCommands(
                profile = profile.toString(),
                platforms = keys.map { key ->
                    val spec = platforms.getValue(key)
                    FfiPlatformImportSpec(
                        platform = key.key,
                        spec = FfiCookieImportSpec(
                            domain = spec.domain,
                            suffixes = spec.suffixes,
                            exactHosts = spec.exactHosts.toList(),
                        ),
                    )
                },
                passwordCommands = passwordCommands,
                passwordTimeoutMs = passwordTimeoutMs,
                nowMs = nowMs,
            )
        } catch (e: uniffi.feedhub_ffi.FfiCookieException) {
            throw java.io.IOException(describe(e), e)
        }
        return toOutcomes(keys, ffi)
    }

    /**
     * 默认的 Chrome / Chromium profile 目录（= 原来 `ChromeCookieImporter.defaultProfile`）。
     *
     * 优先取**真有 `Cookies` 文件**的那个 profile；都没有就退回 `google-chrome/Default`
     * （后面打开时会报"没找到 Chrome 的 Cookie 文件"）。
     */
    fun defaultProfile(
        xdgConfigHome: String? = System.getenv("XDG_CONFIG_HOME"),
        home: String = System.getProperty("user.home"),
    ): java.nio.file.Path {
        RustNative.ensureLoaded()
        return java.nio.file.Paths.get(chromeCookieDefaultProfile(xdgConfigHome, home))
    }

    /**
     * 把 UniFFI 的错误翻成人话。
     *
     * **生成的绑定里 `message` 是字段转储**（`path=…` / `detail=…`，不是 `#[error]` 那句），
     * 所以用户能看到的文案在这里拼——和改动前 Kotlin 抛的那几句话**一字不差**
     * （界面 / cli 都只显示这一句）。**永远不带 Cookie 的值 / 密码 / key。**
     */
    private fun describe(e: uniffi.feedhub_ffi.FfiCookieException): String = when (e) {
        is uniffi.feedhub_ffi.FfiCookieException.ChromeMissingDb ->
            "没找到 Chrome 的 Cookie 文件（${e.path}）"
        is uniffi.feedhub_ffi.FfiCookieException.ChromeSqlite -> "读不了 Chrome 的 Cookie 库：${e.detail}"
        is uniffi.feedhub_ffi.FfiCookieException.ChromeNoKey ->
            "KWallet 和钥匙环里都没取到 Chrome 的密钥（Chrome Safe Storage）"
        is uniffi.feedhub_ffi.FfiCookieException.Storage -> "写 cookies.json 失败：${e.detail}"
        is uniffi.feedhub_ffi.FfiCookieException.CannotQuarantine ->
            "cookies.json 解析失败，也没法留底；为了不覆盖已有登录，不加载也不写回：${e.detail}"
        is uniffi.feedhub_ffi.FfiCookieException.Host -> e.detail
        else -> e.toString()
    }

    private fun toOutcomes(
        keys: List<feedhub.core.Platform>,
        ffi: List<FfiPlatformImportOutcome>,
    ): Map<feedhub.core.Platform, Outcome> {
        val byKey = ffi.associateBy { it.platform }
        return keys.associateWith { key ->
            val raw = byKey[key.key]?.outcome
            if (raw == null) {
                Outcome(0, 0, null, emptyList())
            } else {
                Outcome(
                    imported = raw.imported.toInt(),
                    failed = raw.failed.toInt(),
                    clearDomain = raw.clearDomain,
                    groups = raw.groups.map { g: FfiCookieImportGroup -> g.domain to g.cookies.map(::toCookie) },
                )
            }
        }
    }

    private fun toCookie(c: uniffi.feedhub_ffi.FfiCookieEntry): okhttp3.Cookie = okhttp3.Cookie.Builder()
        .name(c.name)
        .value(c.value)
        .apply { if (c.hostOnly) hostOnlyDomain(c.domain) else domain(c.domain) }
        .path(c.path)
        .expiresAt(c.expiresAtMs)
        .apply { if (c.secure) secure() }
        .apply { if (c.httpOnly) httpOnly() }
        .build()
}
