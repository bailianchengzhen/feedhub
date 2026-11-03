package feedhub.source.xiaohongshu

import feedhub.core.NotLoggedInException
import feedhub.core.QrLogin
import feedhub.core.QrLoginState
import feedhub.core.UserRef

/**
 * **账号层的接缝**："我是谁"和网页扫码登录从哪儿来。
 *
 * 两个实现：
 * - [KotlinXhsAccountApi]：搬进 Rust 之前 `XhsSource.whoami` / `startQrLogin` / `pollQrLogin` 那几段
 *   （一行语义都没改，只是挪过来），是对拍 / 请求等价测试的参照；
 * - `HostedXhsAccountApi`（`XhsHosted.kt`）：**请求 / 签名 / 轮询状态码 / Cookie 落地都在 Rust**
 *   （`feedhub-core/src/xhs/{account,qr}.rs`），Kotlin 只把 record 还原成模型。
 *
 * 三条口径（两个实现必须一样，`XhsAccountEquivalenceTest` 守着）：
 * 1. **成功之前一格 `ctx.cookies` 都不动**：扫码全程用宿主的临时罐（`RustQrHost`），终态才 `commit`；
 * 2. 每次签名都要一组随机量：`start` 只签 1 次，`poll` 最多 3 次（`userinfo` → `status` → `user/me`）；
 * 3. 没有 `a1` 设备 Cookie 时**一个请求都不发**（签名要用它）。
 */
internal interface XhsAccountApi {
    /** 「我是谁」：没登录（没有 `a1` + `web_session`）时 `null`，一个请求都不发。 */
    suspend fun whoami(): UserRef?

    /** 扫码第一步：要一张二维码（没 `a1` 时一个请求都不发、直接报错）。 */
    suspend fun start(): QrLogin

    /** 扫码第二步：问一次状态（终态之后临时罐丢掉）。 */
    suspend fun poll(login: QrLogin): QrLoginState
}

/**
 * Kotlin 参照实现：就是把 `XhsSource` 里原来那三段原样搬过来（`whoami` / `kotlinStartQrLogin` /
 * `kotlinPollQrLogin`），一行语义都没改。它只被对拍 / 请求等价测试用到，生产路径走 `HostedXhsAccountApi`。
 *
 * 为什么不是把这三段留在 `XhsSource` 里直接调：Rust 那条路要**同样的调用点**（`poll` 会签 1～3 次，
 * 每次一组随机量），把接缝抽出来之后两边才是一条形状 —— 请求等价门才比得动。
 */
internal class KotlinXhsAccountApi(private val source: XhsSource) : XhsAccountApi {
    override suspend fun whoami(): UserRef? {
        val api = source.api
        if (!api.hasLogin()) return null
        val json = try {
            api.get("/api/sns/web/v2/user/me")
        } catch (_: NotLoggedInException) {
            return null
        }
        val me = XhsParser.me(json) ?: return null
        api.userId = me.id
        return me
    }

    override suspend fun start(): QrLogin = source.kotlinStartQrLogin()

    override suspend fun poll(login: QrLogin): QrLoginState = source.kotlinPollQrLogin(login)
}
