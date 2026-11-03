package feedhub.proxy

import feedhub.core.ProxyChoice
import feedhub.core.ProxyMode
import feedhub.core.ProxyType
import feedhub.rust.RustNative
import uniffi.feedhub_ffi.FfiProxyChoice
import uniffi.feedhub_ffi.FfiProxyMode
import uniffi.feedhub_ffi.FfiProxyType
import uniffi.feedhub_ffi.proxyBuiltinValid
import uniffi.feedhub_ffi.proxyCustomValid
import uniffi.feedhub_ffi.proxyFallbackReason
import uniffi.feedhub_ffi.proxyModeUnavailable
import uniffi.feedhub_ffi.proxyModeUnavailableReason
import uniffi.feedhub_ffi.proxyResolve

/**
 * **按平台指定代理的判定**：`core/Proxy.kt` 原来的 `resolveProxyMode` /
 * `proxyFallbackReason` / `ProxyMode.unavailable` / `unavailableReason` 现在都在
 * `rust/crates/feedhub-core/src/proxy.rs`，这里是它的 Kotlin 门面。
 *
 * **为什么在 `:rust-bridge`**（和 `feedhub.brain` 一个理由）：`resolveProxyMode` 原来住在 `:core`，
 * 而 `:core` 在依赖顺序上在 `:rust-bridge` 下面（`api(project(":core"))`），调不到 Rust。
 * 所以"要调 Rust 的规则"跟着门面住在这里，App（`app/.../Settings.kt` 的 `AppSettings.proxyFor`）
 * 和 cli（`cli/.../CliProxy.kt` 的 `CliProxyPlan.proxyFor`）都走这一份。
 *
 * **没搬的**：真正建连 / 选 OkHttp 客户端 / 探测连通性仍在宿主（`core/Proxy.kt` 的
 * `PlatformProxySelector` / `ProxyConfigGuardInterceptor` / `toOkHttpProxy`），和"收发留在宿主"一条规矩。
 * 那几处要用"地址填全了没有"来选替身地址，所以 `core` 里留着 `valid`（纯字段检查）；**口径只有一份**：
 * Rust 的 `proxy::custom_valid`，两边由 `ProxyRulesParityTest` 逐个组合守着（含 NBSP / U+0085 这种空白）。
 *
 * **没有开关**：`.so` 加载失败就抛（和 `feedhub.brain` 一样，不悄悄退回 Kotlin 实现）。
 *
 * 线程：纯函数，随调随返（一次调用几微秒），没有阻塞 / 回调 / 后台线程。
 */
object ProxyRules {
    /**
     * 这个平台这次该走什么代理（原 `resolveProxyMode`）。
     *
     * [choice] 是设置里这个平台的那一档，[global] 是「网络」里全局那一档（「内置核心」要用
     * `ProxyCore.proxyMode()` 的结果构造好再传进来），[exitLock] 是「代理不可用时禁止直连」，
     * [overseas] 来自插件描述的 `overseas`。境外平台拿到 [ProxyChoice.Direct] 会按 [ProxyChoice.Follow] 办
     * （红线：境外平台没有「直连」这一档）。
     */
    fun resolve(choice: ProxyChoice, global: ProxyMode, exitLock: Boolean, overseas: Boolean): ProxyMode =
        native { proxyResolve(choice.toFfi(), global.toFfi(), exitLock, overseas).toCore() }

    /**
     * 因为"出口锁关着、全局代理又用不了"而被改成直连时的一句话原因（不含地址）；没有就是 null。
     * 原 `proxyFallbackReason`。单独指定了代理的平台永远是 null（它 fail-closed，不回退）。
     */
    fun fallbackReason(choice: ProxyChoice, global: ProxyMode, exitLock: Boolean): String? =
        native { proxyFallbackReason(choice.toFfi(), global.toFfi(), exitLock) }

    /** 这一档现在用不了（原 `ProxyMode.unavailable`）：指定代理没填全 / 内置核心没在跑。 */
    fun unavailable(mode: ProxyMode): Boolean = native { proxyModeUnavailable(mode.toFfi()) }

    /** [unavailable] 为 true 时的一句话原因（**不含地址 / 端口 / 主机名**，日志和界面直接用）。 */
    fun unavailableReason(mode: ProxyMode): String? = native { proxyModeUnavailableReason(mode.toFfi()) }

    /** 「指定代理」的地址 / 端口填全了没有（`Custom.valid` / `ProxyChoice.Custom.valid` 的口径）。 */
    fun customValid(host: String, port: Int): Boolean = native { proxyCustomValid(host, port) }

    /** 内置代理核心在跑没有（`BuiltinCore.valid` 的口径）。 */
    fun builtinValid(port: Int): Boolean = native { proxyBuiltinValid(port) }

    /**
     * 走 Rust：`.so` 加载失败**不悄悄退回 Kotlin**，给一句说得清的话（和 `feedhub.brain.Brain` 一样）。
     *
     * 这里不像平台对象那样用 `HostedCall.guard`：那是给"会发请求、会抛 `FfiException`"的调用准备的，
     * 这一层是纯函数（没有宿主回调、没有异常类型），失败只剩"`.so` 加载不起来"这一种。
     */
    private inline fun <T> native(block: () -> T): T = try {
        RustNative.ensureLoaded()
        block()
    } catch (e: UnsatisfiedLinkError) {
        throw IllegalStateException("代理判定：Rust 核心加载失败（找不到 libfeedhub_ffi.so）：${e.message}", e)
    }

    // ---------- core 的类型 ↔ FFI 的类型（映射只有这一份） ----------

    private fun ProxyType.toFfi(): FfiProxyType = when (this) {
        ProxyType.HTTP -> FfiProxyType.HTTP
        ProxyType.SOCKS5 -> FfiProxyType.SOCKS5
    }

    private fun FfiProxyType.toCore(): ProxyType = when (this) {
        FfiProxyType.HTTP -> ProxyType.HTTP
        FfiProxyType.SOCKS5 -> ProxyType.SOCKS5
    }

    private fun ProxyMode.toFfi(): FfiProxyMode = when (this) {
        ProxyMode.Direct -> FfiProxyMode.Direct
        ProxyMode.System -> FfiProxyMode.System
        is ProxyMode.Custom -> FfiProxyMode.Custom(host, port, type.toFfi())
        is ProxyMode.BuiltinCore -> FfiProxyMode.BuiltinCore(port, reason)
    }

    /**
     * Rust 给回来的判定。
     *
     * `valid` 不过边界（Rust 现算它自己的那份，宿主拿到之后按同一口径用 `core` 的 `Custom.valid` /
     * `BuiltinCore.valid` 再算一次，建连时用）；两份口径由 [customValid] / [builtinValid] 和
     * `ProxyRulesParityTest` 的全组合表守着。
     */
    private fun FfiProxyMode.toCore(): ProxyMode = when (this) {
        FfiProxyMode.Direct -> ProxyMode.Direct
        FfiProxyMode.System -> ProxyMode.System
        is FfiProxyMode.Custom -> ProxyMode.Custom(host, port, kind.toCore())
        is FfiProxyMode.BuiltinCore -> ProxyMode.BuiltinCore(port, reason)
    }

    private fun ProxyChoice.toFfi(): FfiProxyChoice = when (this) {
        ProxyChoice.Follow -> FfiProxyChoice.Follow
        ProxyChoice.Direct -> FfiProxyChoice.Direct
        is ProxyChoice.Custom -> FfiProxyChoice.Custom(host, port, type.toFfi())
    }
}
