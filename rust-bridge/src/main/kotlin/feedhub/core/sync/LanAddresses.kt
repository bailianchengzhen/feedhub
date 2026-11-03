package feedhub.core.sync

import java.net.Inet4Address
import java.net.NetworkInterface

/**
 * 电脑的局域网地址（发送方选绑定地址用）。
 *
 * 只列**能用手机连上的**：IPv4、网卡 up、非回环、非链路本地（169.254）、RFC1918 私网地址；
 * docker / 虚拟网桥 / VPN / 隧道这类网卡按名字排到最后（还是会列出来，让用户能选——比如手机连的是
 * 某个 VPN 网段时，用户自己知道该选哪个）。界面用 [SyncAddress.label] 给下拉框。
 *
 * "和手机同网段"没法在电脑这边可靠判断（手机还没连上来），所以：自动挑排序第一的（物理无线 / 有线网卡
 * 的私网地址），多个网卡时让用户在下拉框里手动换。这是保守做法：不猜手机在哪，也不在 0.0.0.0 上开服务。
 */
data class SyncAddress(
    /** 二维码里要写的 IPv4 字面量。 */
    val host: String,
    val label: String,
    /** 网卡名（排序 / 排障用，不进二维码）。 */
    val interfaceName: String,
)

object LanAddresses {
    private val VIRTUAL_PREFIXES = listOf(
        "docker", "veth", "virbr", "br-", "vmnet", "vboxnet", "tun", "tap", "tailscale", "wg", "zt", "utun", "singbox",
    )

    /** 按"最可能是手机连着的那个"排序；没有就返回空列表。 */
    fun list(): List<SyncAddress> {
        val result = ArrayList<SyncAddress>()
        val interfaces = try {
            NetworkInterface.getNetworkInterfaces()?.toList().orEmpty()
        } catch (e: Exception) {
            return emptyList()
        }
        for (nif in interfaces) {
            // 安卓模拟器的 eth0 / 有的真机网卡会被标成 virtual，
            // 老实现只在桌面用，把这一条留到安卓就会一个地址都不显示。虚拟 / VPN 网卡本来就排在后面（[score]），
            // 用户仍然看得到、可以自己选。
            val up = runCatching { nif.isUp && !nif.isLoopback }.getOrDefault(false)
            if (!up) continue
            val name = nif.name.lowercase()
            for (address in nif.inetAddresses.toList()) {
                if (address !is Inet4Address) continue
                if (address.isLoopbackAddress || address.isLinkLocalAddress || address.isAnyLocalAddress) continue
                val host = address.hostAddress ?: continue
                if (!isPrivateIpv4(host)) continue
                result += SyncAddress(host, "$host（${nif.name}）", nif.name)
            }
        }
        return result
            .distinctBy { it.host }
            .sortedWith(compareBy({ score(it.interfaceName) }, { it.host }))
    }

    /** 私网 IPv4（10/8、172.16/12、192.168/16）。 */
    fun isPrivateIpv4(host: String): Boolean {
        val parts = host.split('.').mapNotNull { it.toIntOrNull() }
        if (parts.size != 4 || parts.any { it !in 0..255 }) return false
        val (a, b) = parts
        return a == 10 || (a == 172 && b in 16..31) || (a == 192 && b == 168)
    }

    /** 越小越优先：无线 / 有线物理网卡在前，虚拟 / VPN 在后。 */
    internal fun score(interfaceName: String): Int {
        val n = interfaceName.lowercase()
        return when {
            VIRTUAL_PREFIXES.any { n.startsWith(it) } -> 3
            n.startsWith("wl") || n.startsWith("wlan") || n.startsWith("wifi") -> 0
            n.startsWith("en") || n.startsWith("eth") -> 1
            else -> 2
        }
    }
}
