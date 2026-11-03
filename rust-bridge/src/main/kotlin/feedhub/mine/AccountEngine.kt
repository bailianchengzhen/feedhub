package feedhub.mine

import feedhub.core.KeyValueStore
import feedhub.core.Platform
import feedhub.core.UserRef
import feedhub.rust.RustNative
import uniffi.feedhub_ffi.FfiAcctPlan
import uniffi.feedhub_ffi.FfiAcctRow
import uniffi.feedhub_ffi.FfiAcctState
import uniffi.feedhub_ffi.FfiAcctStatus
import uniffi.feedhub_ffi.FfiAcctUser
import uniffi.feedhub_ffi.FfiAcctWhoami
import uniffi.feedhub_ffi.acctPlatformFromStored
import uniffi.feedhub_ffi.acctPlatformToStored
import uniffi.feedhub_ffi.acctSchema
import uniffi.feedhub_ffi.acctStoreKey

/**
 * **账号状态判定 + 本地缓存格式**（`rust/crates/feedhub-core/src/feed/accounts.rs`）的 Kotlin 门面。
 *
 *
 * Kotlin 剩下的：真正的 `whoami()`（OkHttp）、Cookie 罐 / WebView Cookie / 设备码 token 的清写、
 * 浏览器导入、同步包应用、Compose 状态容器、协程。
 *
 * **CACHE_KEY / SCHEMA / toStored / fromStored 都从 Rust 来**（同一份口径），Kotlin 这边不留第二份。
 */
class AccountEngine(platforms: List<Platform>) : AutoCloseable {

    /** 一个平台账号现在的状态。 */
    sealed interface Status {
        data object Checking : Status
        data class LoggedIn(val user: UserRef) : Status
        /** [expired]：本机还留着登录用的 Cookie，但平台说没登录（过期了）；false = 压根没有登录过。 */
        data class LoggedOut(val expired: Boolean) : Status
        /** 查的时候出错了，[message] 是一行原因。 */
        data class Failed(val message: String) : Status
    }

    /**
     * 一次决策：[queries] 是要问的平台（**按这个顺序问**）；[statusChanged] / [checkedAtChanged] 说明
     * 宿主该重抄一次 [status] / [checkedAt]。
     */
    data class Plan(
        val queries: List<Platform>,
        val statusChanged: Boolean,
        val checkedAtChanged: Boolean,
    )

    /** 一次 `whoami()` 的结果（宿主按异常类型分好）。 */
    sealed interface Whoami {
        data class User(val user: UserRef) : Whoami
        /** 平台回了"没有账号"（没登录，但不是异常）。 */
        data object NoUser : Whoami
        /** 抛了 `NotLoggedInException`。 */
        data object NotLoggedIn : Whoami
        /** 别的异常（网络 / 风控 / 接口变了），[message] 是一行原因。 */
        data class Failed(val message: String) : Whoami
    }

    init { RustNative.ensureLoaded() }

    private val native: FfiAcctState = FfiAcctState(platforms.map { it.key })

    /** 进页面：把上次存下来的摆出来、一个请求都不发。[store] 是 `ctx.store`。 */
    fun ensureChecked(store: KeyValueStore): Plan =
        load { native.ensureChecked(store.get(CACHE_KEY)).toCore() }

    /** 用户点了刷新：问全部。 */
    fun refresh(): Plan = load { native.refresh().toCore() }

    /** 问一个平台（登录 / 退出 / 导入 / 同步都走它）。 */
    fun check(platform: Platform): Plan = load { native.check(platform.key).toCore() }

    /**
     * 别处（刷信息流的那个平台）报了"没登录"：标过期、**不发请求**。
     * [hasLoginCookies] 由宿主按插件描述算好（本机有没有它的登录 Cookie）。
     */
    fun markExpired(platform: Platform, hasLoginCookies: Boolean, nowMs: Long): Plan =
        load { native.markExpired(platform.key, hasLoginCookies, nowMs).toCore() }

    /** 一次 `whoami()` 的结果。 */
    fun onWhoami(platform: Platform, outcome: Whoami, hasLoginCookies: Boolean, nowMs: Long): Plan =
        load { native.onWhoami(platform.key, outcome.toFfi(), hasLoginCookies, nowMs).toCore() }

    /** 退出登录：先把确定的"未登录"存下来（万一接下来那次 whoami 断网失败，也不会退回"已登录"）。 */
    fun loggedOut(platform: Platform, nowMs: Long): Plan = load { native.loggedOut(platform.key, nowMs).toCore() }

    /** 把内存里那份存档写回 [store]（`store.put(CACHE_KEY, …)`）。写盘失败由宿主吞掉，不影响界面。 */
    fun save(store: KeyValueStore) {
        RustNative.ensureLoaded()
        store.put(CACHE_KEY, native.encodeStore())
    }

    /** 现在内存里那份存档的文本（测试 / 调试用；写盘用 [save]）。 */
    fun encodeStore(): String = load { native.encodeStore() }

    fun status(): Map<Platform, Status> = load {
        val out = LinkedHashMap<Platform, Status>(16)
        for (row in native.status()) {
            val platform = Platform(row.platform)
            out[platform] = row.status.toCore(platform)
        }
        out
    }

    fun checkedAt(): Long? = load { native.checkedAt() }

    override fun close() {
        native.close()
    }

    private inline fun <T> load(block: () -> T): T {
        RustNative.ensureLoaded()
        return block()
    }

    private fun FfiAcctPlan.toCore(): Plan = Plan(
        queries = queries.map { Platform(it) },
        statusChanged = statusChanged,
        checkedAtChanged = checkedAtChanged,
    )

    private fun FfiAcctStatus.toCore(platform: Platform): Status = when (this) {
        FfiAcctStatus.Checking -> Status.Checking
        is FfiAcctStatus.LoggedIn -> Status.LoggedIn(user.toCore(platform))
        is FfiAcctStatus.LoggedOut -> Status.LoggedOut(expired)
        is FfiAcctStatus.Failed -> Status.Failed(message)
    }

    /**
     * Rust 的账号状态里**带平台**（`app.accounts` 每条的 `user` 里就有它，和 Kotlin 的 `UserRef` 一样），
     * 但状态表本身也是按平台分开存的：两边对不上时以状态表那一格为准（批 id 的来源是它）。
     */
    private fun FfiAcctUser.toCore(platform: Platform): UserRef = UserRef(
        platform = platform,
        id = id,
        name = name,
        avatar = avatar,
        url = url,
    )

    private fun Whoami.toFfi(): FfiAcctWhoami = when (this) {
        is Whoami.User -> FfiAcctWhoami.User(
            FfiAcctUser(user.platform.key, user.id, user.name, user.avatar, user.url),
        )
        Whoami.NoUser -> FfiAcctWhoami.NoUser
        Whoami.NotLoggedIn -> FfiAcctWhoami.NotLoggedIn
        is Whoami.Failed -> FfiAcctWhoami.Failed(message)
    }

    companion object {
        /** 存储里那一项的键（Rust 的 `accounts::STORE_KEY`，**不是** Kotlin 里另写一份）。 */
        val CACHE_KEY: String by lazy {
            RustNative.ensureLoaded()
            acctStoreKey()
        }

        /** 存档格式版本（Rust 的 `accounts::SCHEMA`）。 */
        val SCHEMA: Int by lazy {
            RustNative.ensureLoaded()
            acctSchema()
        }

        /** 写进存储的写法（老的 11 个平台写 enum 名）。 */
        fun toStored(platform: Platform): String {
            RustNative.ensureLoaded()
            return acctPlatformToStored(platform.key)
        }

        /** 存储里的写法 → 平台。 */
        fun fromStored(text: String): Platform {
            RustNative.ensureLoaded()
            return Platform(acctPlatformFromStored(text))
        }
    }
}
