package feedhub.app.sync

/**
 * 「登录互传」这个端能做什么。
 *
 * 任意两台客户端扫码互传：**出码方**起一次性服务（选「发出去」或「收进来」），**扫码方**连过去。
 * 电脑没摄像头 → 电脑永远出码；手机 / 平板两种都能当。两端在组装 `AppGraph` 时给一份，
 * 为 null = 这个组装不支持（演示 / 截图工具）。
 *
 * @param manualQrInput 是否允许手动粘贴二维码内容代替摄像头。**只在 debug 构建里为 true**：
 *   模拟器没有可用摄像头、真机又吞 adb 注入的触摸时用它（页面里会写清"含一次性密钥，别截图外发"）。
 */
data class LoginSync(
    /** 这台设备能不能出码（电脑只能出码）。 */
    val canOffer: Boolean = false,
    /** 这台设备能不能扫码（手机 / 平板有摄像头）。 */
    val canScan: Boolean = false,
    val manualQrInput: Boolean = false,
)
