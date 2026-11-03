package feedhub.app.login

import java.util.concurrent.atomic.AtomicBoolean

/**
 * 「WebView 现在有没有进程级的代理覆盖」的标记。
 *
 * 为什么要它：`androidx.webkit.ProxyController` 的覆盖是**进程级**的（不是某个 WebView 的），
 * 网页登录页给境外平台设置代理后关掉时会清（见 `WebLoginScreen.android.kt` 的 `clearWebViewProxy`），
 * 但清是异步的、而且用户可能**正开着登录页**。
 *
 * 酷安是国内平台，验证码 WebView 必须走设备自己的网络：如果这时后台登录页的覆盖还在，验证页就会跟着走境外代理
 * （或者干脆加载不出来）。所以验证码 handler 弹页面前先问这里，还开着就**先不弹**（见
 * `AndroidCaptchaHandler.refuseReason`）——宁可让用户关掉登录页再重试，也不让国内验证码页面从境外代理出去，
 * 更不悄悄清掉登录页正在用的覆盖（那会把境外登录页变成直连，撞红线）。
 *
 * 只反映"登录页设过 / 清过"，不代表覆盖真的已经生效 / 已撤销（系统没有查询接口）；这是保守方向的近似。
 */
object WebViewProxyGate {
    private val active = AtomicBoolean(false)

    /** 现在有（或刚有过）进程级代理覆盖。 */
    val isActive: Boolean get() = active.get()

    /** 登录页设好覆盖 / 清掉覆盖时调。 */
    fun setActive(value: Boolean) {
        active.set(value)
    }
}
