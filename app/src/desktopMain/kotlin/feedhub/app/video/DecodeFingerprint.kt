package feedhub.app.video

import feedhub.player.RustGstRuntime

/**
 * 这台机器**解码环境**的指纹：选定 VA 设备的 PCI `vendor:device` + libva 报的驱动串（Mesa 版本）+
 * GStreamer 版本。`DecodeLedger` 拿它当落盘记录的失效判据：任何一项变了（换显卡 / 插拔 eGPU /
 * 升级 Mesa / 升级 GStreamer），旧记录整份作废，重新学。
 *
 * 拿不到就返回 null（没装 GStreamer、VA-API 起不来、sysfs 读不到）：那时候**不读也不写**账本 ——
 * 宁可不落盘，也不拿一份不知道对谁有效的记录去选流。
 *
 * 读它会触发一次 GStreamer 的惰性探测（挑设备 + 问 libva），之后都是缓存值；所以不要在启动路径上调。
 *
 * 这里只是过一层边界——两边的字符串逐字节相同（账本的落盘记录靠它判断失效）。
 */
internal fun decodeEnvironmentFingerprint(): String? = RustGstRuntime.decodeEnvironmentFingerprint()
