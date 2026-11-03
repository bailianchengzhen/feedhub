package feedhub.source.douyin

import feedhub.rust.RustDouyin
import feedhub.rust.RustDouyinHosted

/**
 * Kotlin 的 a_bogus 常量 / 浏览器环境 / 设备值 → `rust-bridge` 的门面类型。
 *
 * 为什么单独一个文件：签名（[RustDouyinSign]）和平台对象（[RustDouyinHosted]）都要把**同一套**值交给 Rust，
 * 转换写两份迟早会漂——漂了就是"签名的 UA"和"请求头的 UA"不一致，a_bogus 直接废、请求也自相矛盾。
 * 这里只是**搬**（一个字段都不生成、不改写）。
 */

/** `abogus_profile.json` / `ctx.store` 的那份常量 → Rust 的签名参数。 */
internal fun AbogusProfile.toRustProfile(): RustDouyin.AbogusProfile = RustDouyin.AbogusProfile(
    aid = aid,
    pageId = pageId,
    sdkVersion = sdkVersion,
    sdkMinor = sdkMinor,
    u17 = u17,
    u37 = u37.toList(),
    counter = counter,
)

/** `DouyinDevice.abogusEnv` → Rust 的签名环境。 */
internal fun AbogusEnv.toRustEnv(): RustDouyin.AbogusEnv = RustDouyin.AbogusEnv(
    userAgent = userAgent,
    platform = platform,
    innerWidth = innerWidth,
    innerHeight = innerHeight,
    outerWidth = outerWidth,
    outerHeight = outerHeight,
    availWidth = availWidth,
    availHeight = availHeight,
    screenSizeWidth = screenSizeWidth,
    screenSizeHeight = screenSizeHeight,
    fingerprintOverride = fingerprintOverride,
)

/**
 * `DouyinDevice` 的那组环境值 → 平台对象的设备参数。
 *
 * 设备身份**只有这一份**：`install_time` / `verify_fp` 是 `DouyinDevice` 从 `ctx.store` 取的那一份
 * （没有时它生成一次并存进去），Rust 拿到什么就用什么，不新造。
 */
internal fun DouyinDevice.toRustDevice(): RustDouyinHosted.Device = RustDouyinHosted.Device(
    userAgent = userAgent,
    platform = platform,
    uaPlatform = uaPlatform,
    osName = osName,
    osVersion = osVersion,
    libraDivert = libraDivert,
    browserMajor = browserMajor,
    screenWidth = screenWidth,
    screenHeight = screenHeight,
    windowWidth = windowWidth,
    windowHeight = windowHeight,
    cpuCoreNum = cpuCoreNum,
    deviceMemory = deviceMemory,
    installTime = installTime,
    verifyFp = verifyFp,
)
