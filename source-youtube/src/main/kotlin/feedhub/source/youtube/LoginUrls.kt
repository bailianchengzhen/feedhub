package feedhub.source.youtube

/** 设备码要用到的四个地址（真实值；测试换成 MockWebServer 的 http 地址）。 */
data class LoginUrls(
    val tvDeviceCode: String = "https://www.youtube.com/o/oauth2/device/code",
    val tvToken: String = "https://www.youtube.com/o/oauth2/token",
    val googleDeviceCode: String = "https://accounts.google.com/o/oauth2/device/code",
    val googleToken: String = "https://www.googleapis.com/oauth2/v4/token",
) {
    companion object {
        val DEFAULT = LoginUrls()
    }
}
