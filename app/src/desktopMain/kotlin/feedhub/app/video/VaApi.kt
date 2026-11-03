package feedhub.app.video

import com.sun.jna.Library
import com.sun.jna.Native
import com.sun.jna.Pointer
import com.sun.jna.ptr.IntByReference
import feedhub.core.VideoCodec
import feedhub.player.RustGstRuntime
import java.io.File

/**
 * libva 的最小 JNA 绑定：只用来**问**某个 render node 支持哪些 profile / entrypoint（等价 `vainfo --display drm --device`），
 * 不建解码器、不解码。用来把"元素建得出来"升级成"设备侧确认有 VLD 解码入口点"，见 Rust 的 `gst_engine::runtime::probe_va`。
 *
 * 为什么不用 `vainfo`：这台机器上没装（`gstreamer1.0-va` / `vainfo` 包都没装），而 libva 本体和驱动都在
 * （`libva.so.2` / `libva-drm.so.2` + `radeonsi_drv_video.so`），直接调它最省事、也不用 sudo 装东西。
 * 装了 vainfo 的好处只是排查时能一条命令看全部 profile，本文件的 [query] 输出的就是同一份信息。
 */
internal object VaApi {
    /** layout 里 `drm-engine-*` 之类没用；这里只要 profile 和 entrypoint。 */
    private interface Libva : Library {
        fun vaInitialize(dpy: Pointer, major: IntByReference, minor: IntByReference): Int
        fun vaTerminate(dpy: Pointer): Int
        fun vaQueryVendorString(dpy: Pointer): String?
        fun vaMaxNumProfiles(dpy: Pointer): Int
        fun vaQueryConfigProfiles(dpy: Pointer, list: IntArray, n: IntByReference): Int
        fun vaMaxNumEntrypoints(dpy: Pointer): Int
        fun vaQueryConfigEntrypoints(dpy: Pointer, profile: Int, list: IntArray, n: IntByReference): Int
        fun vaProfileStr(profile: Int): String?
        fun vaEntrypointStr(entrypoint: Int): String?
    }

    /** `vaGetDisplayDRM` 在 libva-drm.so.2 里，其余在 libva.so.2。 */
    private interface LibvaDrm : Library {
        fun vaGetDisplayDRM(fd: Int): Pointer?
    }

    private interface Libc : Library {
        fun open(path: String, flags: Int): Int
        fun close(fd: Int): Int
    }

    /** VAEntrypointVLD：硬件解码入口点。 */
    const val ENTRYPOINT_VLD = 1

    /**
     * 每种编码在 VA-API 里可能用的 profile id（任一有 VLD 就算这块卡能解这种编码）。
     *
     * 和这里是同一份），
     * 这个表留着给 [VaApi.Device.supportsDecode] 用（探针打印用，不参与播放决策）。
     */
    private val PROFILES = mapOf(
        VideoCodec.AVC to listOf(7, 6, 13),      // H264High / H264Main / H264ConstrainedBaseline
        VideoCodec.HEVC to listOf(17, 18),       // HEVCMain / HEVCMain10
        VideoCodec.AV1 to listOf(32, 33),        // AV1Profile0 / AV1Profile1
    )

    private val libva: Libva? by lazy { runCatching { Native.load("va", Libva::class.java) }.getOrNull() }
    private val libvaDrm: LibvaDrm? by lazy { runCatching { Native.load("va-drm", LibvaDrm::class.java) }.getOrNull() }
    private val libc: Libc? by lazy { runCatching { Native.load("c", Libc::class.java) }.getOrNull() }

    /** 一个 render node 上 VA-API 报出来的能力。 */
    class Device(
        val path: String,
        val vendor: String?,
        /** libva 报的驱动串（如 "Mesa Gallium driver … for AMD Radeon AMD 核显"）。 */
        val driver: String,
        val vaVersion: String,
        /** profile id → 它的 entrypoint 列表。 */
        val profiles: Map<Int, List<Int>>,
    ) {
        /** 这种编码在这块卡上有没有 VLD（真解码）入口点。 */
        fun supportsDecode(codec: VideoCodec): Boolean =
            PROFILES[codec].orEmpty().any { p -> profiles[p]?.contains(ENTRYPOINT_VLD) == true }

        /** 人看得懂的 profile 清单。 */
        fun profileText(): String = profiles.entries.joinToString(", ") { (p, eps) ->
            "${profileStr(p) ?: "p$p"}[${eps.joinToString("/") { entrypointStr(it) ?: "ep$it" }}]"
        }

        override fun toString() = "$path（${RustGstRuntime.vendorName(vendor)}，$driver，VA-API $vaVersion）"
    }

    /**
     * 问 [path] 这块卡支不支持 VA-API。`vaInitialize` 失败返回 null（比如驱动没装、节点不是自己的）。
     * 只做查询，不做任何解码 / 计算。
     */
    fun query(path: String): Device? {
        val va = libva ?: return null
        val drm = libvaDrm ?: return null
        val c = libc ?: return null
        if (!File(path).exists()) return null
        val fd = c.open(path, O_RDWR)
        if (fd < 0) return null
        val dpy = drm.vaGetDisplayDRM(fd) ?: run { c.close(fd); return null }
        val major = IntByReference()
        val minor = IntByReference()
        if (va.vaInitialize(dpy, major, minor) != 0) {
            c.close(fd)
            return null
        }
        try {
            val profiles = LinkedHashMap<Int, List<Int>>()
            val maxP = va.vaMaxNumProfiles(dpy)
            if (maxP > 0) {
                val arr = IntArray(maxP)
                val n = IntByReference()
                if (va.vaQueryConfigProfiles(dpy, arr, n) == 0) {
                    for (i in 0 until n.value) {
                        val p = arr[i]
                        val maxE = va.vaMaxNumEntrypoints(dpy)
                        if (maxE <= 0) continue
                        val eps = IntArray(maxE)
                        val en = IntByReference()
                        if (va.vaQueryConfigEntrypoints(dpy, p, eps, en) == 0) profiles[p] = (0 until en.value).map { eps[it] }
                    }
                }
            }
            return Device(
                path = path,
                vendor = RustGstRuntime.vendorOf(path),
                driver = runCatching { va.vaQueryVendorString(dpy) ?: "" }.getOrDefault(""),
                vaVersion = "${major.value}.${minor.value}",
                profiles = profiles,
            )
        } finally {
            runCatching { va.vaTerminate(dpy) }
            c.close(fd)
        }
    }

    fun profileStr(id: Int): String? = runCatching { libva?.vaProfileStr(id) }.getOrNull()
    fun entrypointStr(id: Int): String? = runCatching { libva?.vaEntrypointStr(id) }.getOrNull()

    private const val O_RDWR = 2
}

/**
 * VA-API 的 profile 口径（给 `dev/VaProbe` 判定"这块卡能不能解某种编码"用）。
 *
 * 真值在 Rust 的 `gst_engine::vaapi::profiles_for`（生产路径用它判定硬解），这里只是同一份的
 * Kotlin 副本：探针打印每块卡的 profile 时不必为了一个查表再过边界。
 */
internal object VaProfiles {
    /** 每种编码可能用的 profile id。 */
    fun forCodec(codec: feedhub.core.VideoCodec): List<Int> = when (codec) {
        feedhub.core.VideoCodec.AVC -> listOf(7, 6, 13)   // H264High / H264Main / H264ConstrainedBaseline
        feedhub.core.VideoCodec.HEVC -> listOf(17, 18)    // HEVCMain / HEVCMain10
        feedhub.core.VideoCodec.AV1 -> listOf(32, 33)     // AV1Profile0 / AV1Profile1
        feedhub.core.VideoCodec.OTHER -> emptyList()
    }
}
