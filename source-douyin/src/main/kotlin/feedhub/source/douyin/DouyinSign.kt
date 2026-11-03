package feedhub.source.douyin

import feedhub.core.Platform
import feedhub.rust.RustDouyin

/**
 * "签名谁来算"这一个接缝（a_bogus + `x-secsdk-web-signature`）。
 *
 * 两个实现：
 * - [KotlinDouyinSign]：现有的 [AbogusSigner] / [SecSdkSigner]，行为和这个功能存在之前**逐字节一样**；
 * - [RustDouyinSign]：同样的算法在 Rust 里（`rust/crates/feedhub-core/src/douyin/sign.rs`），经 UniFFI 过来。
 *
 * 签名在 OkHttp 拦截器里算（`DouyinSource.SignInterceptor`，排在限速**之后**），所以这个接口是同步的、
 * 被拦截器线程直接调。**请求本身一个字节都不换**：拼 URL / 参数 / 头还在 `DouyinSource` 里。
 *
 * 时间（`now` / `enterPageTs`）由调用方注入；a_bogus 要的随机数由实现自己按**和 Kotlin 版相同的顺序**
 * 从同一个 [AbogusRandom] 里取（Rust 版先问 [`RustDouyin.abogusRandomCount`] 要几个），
 * 所以两边签出来逐字节相同，生产的随机性也和原来一样。
 */
internal interface DouyinSign {
    /**
     * 生成 a_bogus。
     *
     * @param uri 已经百分号编码、不含 `a_bogus` 自己的 query 串
     * @param now 这一刻（`clock()`；由调用方取一次，两边共用同一个值）
     *            `enterPageTs`（进入页面的时间，只参与 `U[28]`）在建签名器时就定下了，不走参数——
     *            免得两个实现各拿一份、悄悄对不上。
     */
    fun abogus(uri: String, now: Long): String

    /** 受保护路径的 `timestamp` + `x-secsdk-web-signature`；返回签名值和**实际要发出去的** query。 */
    fun secSdk(query: String, uifidFallback: String?, timestampSec: Long): SecSdkSigned
}

/** 一次 secsdk 签名的结果（不带 timestamp：调用方已经知道）。 */
internal class SecSdkSigned(val signature: String, val query: String)

/** Kotlin 自己的签名（默认）。就是原来那两行。 */
internal class KotlinDouyinSign(private val abogus: AbogusSigner) : DouyinSign {
    override fun abogus(uri: String, now: Long): String = abogus.sign(uri, now)

    override fun secSdk(query: String, uifidFallback: String?, timestampSec: Long): SecSdkSigned {
        val signed = SecSdkSigner.sign(query, uifidFallback, timestampSec)
        return SecSdkSigned(signed.signature, signed.query)
    }
}

/**
 * Rust 签名。常量和浏览器环境由 Kotlin 读好传进来（`abogus_profile.json` / `DouyinDevice`），
 * 时间由调用方给，随机数由这里从 [random] 里按 Kotlin 版的顺序取——**Rust 不取时钟、不取随机数**。
 */
internal class RustDouyinSign(
    profile: AbogusProfile,
    env: AbogusEnv,
    private val random: AbogusRandom,
    private val enterPageTs: Long,
    private val platform: Platform = DOUYIN,
) : DouyinSign {
    // 常量 / 环境的转换和平台对象（`RustDouyinHosted`）共用一份，见 `DouyinRustBridge.kt`
    private val ffiProfile = profile.toRustProfile()
    private val ffiEnv = env.toRustEnv()

    override fun abogus(uri: String, now: Long): String {
        // 先问"这次要取几个随机数"（纯计算：指纹串 / 时间串的长度决定 u88 有几段），
        // 再按 Kotlin 版**同样的顺序**取——两边的随机流就完全对齐了。
        val count = RustDouyin.abogusRandomCount(ffiEnv, now, platform)
        val rand = DoubleArray(count) { random.nextDouble() }
        return RustDouyin.abogusSign(ffiProfile, ffiEnv, uri, now, enterPageTs, rand, platform)
    }

    override fun secSdk(query: String, uifidFallback: String?, timestampSec: Long): SecSdkSigned {
        val signed = RustDouyin.secSdkSign(query, uifidFallback, timestampSec, platform)
        return SecSdkSigned(signed.signature, signed.query)
    }
}
