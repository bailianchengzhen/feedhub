package feedhub.app.sync

/**
 * 「配对新设备」在这台设备上能做什么（桌面 / 安卓各自组装时给一份；null = 不显示入口）。
 *
 * - [canHost]：能显示二维码、开一次性局域网服务（桌面 / 安卓都有）；
 * - [canScan]：能用摄像头扫码（安卓；桌面没有摄像头，用 [manualQrInput] 粘二维码内容）；
 * - [manualQrInput]：能手动粘二维码内容（桌面 / debug 安卓；内容里有一次性密钥，界面会写警告）。
 */
data class SyncPairingUi(
    val canHost: Boolean = false,
    val canScan: Boolean = false,
    val manualQrInput: Boolean = false,
)
