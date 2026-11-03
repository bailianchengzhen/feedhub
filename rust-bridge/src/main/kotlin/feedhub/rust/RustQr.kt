package feedhub.rust

import uniffi.feedhub_ffi.FfiQrJsonError
import uniffi.feedhub_ffi.FfiQrSessions
import uniffi.feedhub_ffi.qrDefaultMaxSize
import uniffi.feedhub_ffi.qrParseObject

/**
 * 扫码登录共用件的 Rust 门面。
 *
 * 两个东西，都不发请求：
 * - [sessions]：扫码会话的 **LRU**（Kotlin `QrSessions` 的 `LinkedHashMap(accessOrder = true)` 那条规则）。
 *   **Cookie 罐（`MemoryCookieJar`）在宿主**，Rust 只记"谁排在哪、谁该被挤掉"，门面把被挤掉的键交回去；
 * - [parse]：二维码接口响应的解析 + 判错（是 JSON 对象 / 是 JSON 但不是对象 / 不是 JSON）。
 *
 * 这个文件住在 `:rust-bridge`，但 `QrSessions` / `parseQrJson` 的包名仍是 `feedhub.core`
 * （照 `MinIntervalInterceptor` 的先例）：`:core` 编译期看不见本模块，而这两个入口被 `source-*` 的
 * 十几处调用——整类搬过来、包名不变，调用方一行都不用改。
 */
internal object RustQr {
    /** `QrSessions()` 的默认容量（**只有** Rust 那一份是真相；Kotlin 侧用常量接住，有测试钉着两边相等）。 */
    fun defaultMaxSize(): Int = load { qrDefaultMaxSize().toInt() }

    /** 一个扫码会话的 LRU（容量 [maxSize]）。 */
    fun sessions(maxSize: Int): FfiQrSessions = load { FfiQrSessions(maxSize.toUInt()) }

    /** 解析结果：成功时 [json] 是紧凑 JSON，失败时 [error] 说是哪一档。 */
    class Parsed(val json: String?, val error: Error) {
        enum class Error {
            /** 解析成功。 */
            NONE,

            /** 不是合法 JSON。 */
            INVALID,

            /** 是 JSON，但不是对象（数组 / 标量）。 */
            NOT_OBJECT,
        }
    }

    /** 二维码接口的响应 → 紧凑 JSON（判错在 Rust；调用方按 [Parsed.Error] 拼文案，**不带响应内容**）。 */
    fun parse(body: String): Parsed = load {
        val result = qrParseObject(body)
        Parsed(
            json = result.json,
            error = when (result.error) {
                FfiQrJsonError.NONE -> Parsed.Error.NONE
                FfiQrJsonError.INVALID -> Parsed.Error.INVALID
                FfiQrJsonError.NOT_OBJECT -> Parsed.Error.NOT_OBJECT
            },
        )
    }

    /** `.so` 的加载在**第一次用的时候**（构造 `QrSessions` 不碰 Rust，失败也就不会发生在平台的构造函数里）。 */
    private inline fun <T> load(block: () -> T): T {
        RustNative.ensureLoaded()
        return block()
    }
}
