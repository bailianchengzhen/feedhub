package feedhub.core

import feedhub.rust.RustQr
import kotlinx.coroutines.CancellationException
import kotlinx.serialization.json.JsonObject
import okhttp3.Cookie
import okhttp3.HttpUrl.Companion.toHttpUrl
import java.io.IOException

/**
 * 扫码登录各平台共用的小工具（接口见 [QrLoginSource]）。
 *
 * 扫码流程不碰 `ctx.cookies`：每次 [QrLoginSource.startQrLogin] 在 [QrSessions] 里开一个独立的临时 Cookie 罐，
 * 生成二维码、轮询用的客户端都是 `http.newBuilder().cookieJar(临时罐)` 派生的；只有 [commitQrLogin] 在成功时才写 `ctx.cookies`。
 *
 * 这个文件从 `:core` 搬到 `:rust-bridge`、**包名不变**
 * （照 `MinIntervalInterceptor` 的先例）：`:core` 编译期看不见 `:rust-bridge`，而这两个入口被
 * `source-*` 的十几处调用——整类搬过来，调用方一行都不用改。`withCookieJar` / `commitQrLogin` 是
 * 纯宿主动作（换 OkHttp 客户端、读写 Cookie 罐），留在这一份里原样不动。
 */
class QrSessions(private val maxSize: Int = DEFAULT_MAX_SIZE) {
    /**
     * 临时罐本身（**宿主对象**，Rust 那边只有"谁排在哪"）。键是平台给的 token。
     *
     * 只被 [begin] / [jar] / [end] 在 `@Synchronized` 里碰，和原来的 `LinkedHashMap` 是同一份所有权。
     */
    private val jars = LinkedHashMap<String, MemoryCookieJar>()

    /** Rust 那边的 LRU。**第一次用才加载 `.so`**：构造 `QrSessions` 不该失败在"库没编出来"上。 */
    private val lru by lazy(LazyThreadSafetyMode.SYNCHRONIZED) { RustQr.sessions(maxSize) }

    /**
     * 把 [jar] 登记为 [token] 的临时罐（同一个 token 重复登记会覆盖）。生成二维码那一步先用一个空罐发请求
     * （百度的 BAIDUID、知乎的 d_c0 都是这一步种的），拿到 token 后再登记，轮询时接着用。
     */
    @Synchronized
    fun begin(token: String, jar: MemoryCookieJar = MemoryCookieJar()): MemoryCookieJar {
        lru.begin(token)?.let { jars.remove(it) }
        jars[token] = jar
        return jar
    }

    /** 取 [token] 的临时罐；进程重启 / 换了 Source 实例找不到时给一个空的（轮询仍可继续，只是没有生成时的 Cookie）。 */
    @Synchronized
    fun jar(token: String): MemoryCookieJar {
        val slot = lru.jar(token)
        slot.evicted?.let { jars.remove(it) }
        return jars.getOrPut(token) { MemoryCookieJar() }
    }

    /** 终态（成功 / 过期 / 失败）后丢掉。 */
    @Synchronized
    fun end(token: String) {
        lru.end(token)
        jars.remove(token)
    }

    companion object {
        /**
         * [QrSessions] 的默认容量（8）。**真相在 Rust**（`qr::sessions::DEFAULT_MAX_SIZE`），
         * 这里只是构造函数的默认参数——`QrSessions()` 被平台 Source 当字段建在构造函数里，
         * 不想为了读一个常量就加载 `.so`。`RustQrSessionsTest` 钉着两边相等。
         */
        const val DEFAULT_MAX_SIZE: Int = 8
    }
}

/**
 * 同一个 http / store，换一个 Cookie 罐：用临时罐验证新登录态时，把现成的 Source 实现套上去就行。
 * [platform] 用来带上传这个平台该走的代理（临时客户端也要走）。
 */
fun SourceContext.withCookieJar(platform: Platform, jar: FeedCookieJar): SourceContext =
    SourceContext(client(platform).cookieJar(jar).build(), jar, store, proxy)

/**
 * 二维码相关接口的 JSON 解析：**解析 + 判错在 Rust**（`qr::parse`），这里只把紧凑 JSON 变成
 * `JsonObject`（宿主的模型表示）。出错信息里**不带响应内容**（成功的响应里有凭据）。
 */
fun parseQrJson(platform: Platform, body: String): JsonObject {
    val parsed = RustQr.parse(body)
    val text = parsed.json ?: throw ParseException(
        platform,
        when (parsed.error) {
            RustQr.Parsed.Error.NOT_OBJECT -> "二维码接口的响应不是 JSON 对象"
            RustQr.Parsed.Error.INVALID -> "二维码接口的响应不是合法 JSON"
            RustQr.Parsed.Error.NONE -> "二维码接口的响应不是合法 JSON"
        },
    )
    return try {
        FeedJson.parseToJsonElement(text) as? JsonObject
            ?: throw ParseException(platform, "二维码接口的响应不是 JSON 对象")
    } catch (e: SourceException) {
        throw e
    } catch (e: Exception) {
        throw ParseException(platform, "二维码接口的响应不是合法 JSON")
    }
}

/**
 * 扫码成功后的收尾，四个平台共用：
 * 1. [verify] 用**临时罐**问一次"现在是谁"（一般就是用临时罐派生一个 Source 调 `whoami()`）。
 *    返回 null 或抛 [NotLoggedInException]：平台说成功了但凭据其实不能用 → [QrLoginState.Failed]，**现有 Cookie 一个都不动**。
 *    网络错误、风控等其他错误：没法验证，但平台已经明确说登录成功（这个二维码也用掉了），所以照样写入，`user` 留空让前端再问。
 * 2. 把 [loginNames] 这些旧的登录 Cookie 从 [LoginSpec.cookieDomain] 下删掉（设备标识类 Cookie 不在里面，不动），
 *    再写入 [fresh]（同名同域同路径的会替换）。
 */
suspend fun SourceContext.commitQrLogin(
    login: LoginSpec,
    loginNames: Set<String>,
    fresh: List<Cookie>,
    verify: suspend () -> UserRef?,
): QrLoginState {
    if (fresh.isEmpty()) return QrLoginState.Failed("登录成功了但没拿到任何 Cookie，现有登录态没有改动")
    val user = try {
        verify() ?: return QrLoginState.Failed("登录成功了但验证没通过（平台说没登录），现有登录态没有改动")
    } catch (e: CancellationException) {
        throw e
    } catch (_: NotLoggedInException) {
        return QrLoginState.Failed("登录成功了但验证没通过（平台说没登录），现有登录态没有改动")
    } catch (_: SourceException) {
        null
    } catch (_: IOException) {
        null
    }
    cookies.remove(login.cookieDomain, loginNames)
    cookies.put(login.cookieUrl.toHttpUrl(), fresh)
    return QrLoginState.Success(user)
}
