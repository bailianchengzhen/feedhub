package feedhub.source.xiaohongshu

import java.math.BigDecimal
import kotlin.random.Random

/**
 * 小红书网页接口的请求签名：`X-s`（`XYS_…`）、`X-t`、`X-S-Common`、两个 traceid，以及 homefeed / feed / user_posted 要带的
 * `x-rap-param` 和 homefeed / feed 要带的 `xy-direction`。纯函数 + 一点会话计数，没有网络。
 *
 * 现网格式：
 * - `X-s` = `XYS_` + Base64(JSON{x0:签名版本, x1:appId, x2:平台, x3:`mns0301_…`, x4:"object"(POST 有请求体)|""(GET), x5:MD5(内容)的十六进制})，
 *   **x6 / x7 不带**（它们只在页面拿到 localStorage 里的 `webSsk` 之后才有，我们没有）；
 * - x3 = `mns0301_` + 自定义 Base64(144 字节明文 ⊕ 固定密钥流)，明文布局见 [XhsMns.pack]；
 * - `X-S-Common` = Base64(JSON{s0..x12})，x8 是 b1、x9 是 b1 的 CRC32 变体、x12 是 `<dsllt>;<_dsl>`；
 * - 签名的"内容"是 `api + 请求体`：POST 的 api 是路径、请求体是紧凑 JSON（和实际发出去的逐字节一致）；
 *   GET 的 api 是路径 + `?` + 查询串（和实际发出去的逐字节一致），没有请求体。
 *
 * 参考：
 * - cv-cat/Spider_XHS（同一作者已把这些脚本以 MIT 收进 cv-cat/catbus 的 `static/xhs/`）：`xhs_core/js/mns.js`（x3 明文布局、三档）、`sign.js`（X-s 外壳、x5）、`xs_common.js`（X-S-Common、x9）、`b1.js`（b1），
 *   `xhs_pc/params.py`（请求头、traceid、xy-direction）、`xhs_pc/state.py`（会话计数：seq、dsllt、稳态用 0301 这一档；
 *   这两份 Python 在 catbus 里是 `src/platforms/xhs/web/` 的 TS 版）；
 * - xhshow（MIT）`core/crypto.py:build_payload_array`（x3 明文布局，和上面一致；环境位那 14 字节里它是旧版 0x83、新版是 0x81）、
 *   `core/common_sign.py`、`generators/fingerprint.py:generate_b1`（RC4 → UTF-8 的字节化方式）、`utils/random_gen.py`（traceid）。
 */
internal class XhsSigner(
    private val profile: XhsProfile,
    private val random: Random = Random.Default,
) {
    /** 一个待签名的请求。 */
    class Request(
        /** 签名用的 api：POST 是路径，GET 是路径 + `?` + 查询串。 */
        val api: String,
        /** POST 的紧凑 JSON；GET 为 null。 */
        val body: String?,
        val a1: String,
        /** Cookie 里的 `loadts`（页面加载时间，毫秒）。 */
        val loadtsMs: Long,
        val webBuild: String,
        val b1: String,
        /** 用户 id，算 `xy-direction`；不知道传 null（随机一个 10~100 的值）。 */
        val userId: String?,
        val rap: Boolean,
        val xy: Boolean,
    )

    private var seq = 12 + random.nextInt(29)
    private var signCount = 5 + random.nextInt(60)
    private var dsllt = 0L
    private var xraySeq = random.nextInt(0x7fffff)

    /** 签一个请求，返回要加的请求头（小写名字）。[nowMs] 给测试固定时间。 */
    @Synchronized
    fun sign(req: Request, nowMs: Long = System.currentTimeMillis()): LinkedHashMap<String, String> {
        val release = profile.release
        val content = req.api + (req.body ?: "")
        val md5Full = XhsCodec.md5(content)
        val plain = XhsMns.pack(
            version = random.nextInt(),
            tsMs = nowMs,
            loadtsMs = req.loadtsMs,
            seq = ++seq,
            envConst = profile.mns.envConst,
            content = content,
            api = req.api,
            a1 = req.a1,
            appId = release.appId,
            envFpTail = profile.mns.envFpTail,
        )
        val x3 = XhsMns.x3(plain, profile.mns.keystream)
        val xs = XhsMns.xs(
            signVersion = release.signVersion,
            appId = release.appId,
            platform = release.platform,
            x3 = x3,
            x4 = if (req.body == null) "" else "object",
            x5 = XhsCodec.hex(md5Full),
        )
        if (dsllt == 0L || nowMs - dsllt >= DSL_REFRESH_MS) dsllt = nowMs
        val common = xsCommon(req, "$dsllt;${profile.dsl}", ++signCount)

        val headers = LinkedHashMap<String, String>()
        // 没有 user_id（还没调过 whoami）时用 a1 当种子：不是真值，但同一个设备每次一样
        if (req.xy) headers["xy-direction"] = xyDirection(req.userId ?: req.a1).toString()
        headers["x-xray-traceid"] = xrayTraceId(nowMs)
        headers["x-t"] = nowMs.toString()
        headers["x-b3-traceid"] = hexRandom(16)
        headers["x-s-common"] = common
        if (req.rap) headers["x-rap-param"] = XhsRap.generate(profile.rap, req.api, req.body ?: "", nowMs, random)
        headers["x-s"] = xs
        return headers
    }

    /** X-S-Common：字段顺序和网页一致，JSON 紧凑。 */
    fun xsCommon(req: Request, x12: String, x10: Int): String {
        val r = profile.release
        val json = buildString {
            append("{\"s0\":5,\"s1\":\"\",\"x0\":\"1\",\"x1\":").append(XhsCodec.jsonString(r.signVersion))
            append(",\"x2\":").append(XhsCodec.jsonString(r.platform))
            append(",\"x3\":").append(XhsCodec.jsonString(r.appId))
            append(",\"x4\":").append(XhsCodec.jsonString(req.webBuild))
            append(",\"x5\":").append(XhsCodec.jsonString(req.a1))
            append(",\"x6\":\"\",\"x7\":\"\",\"x8\":").append(XhsCodec.jsonString(req.b1))
            append(",\"x9\":").append(XhsCodec.crc32Variant(req.b1))
            append(",\"x10\":").append(x10)
            append(",\"x11\":\"normal\",\"x12\":").append(XhsCodec.jsonString(x12))
            append('}')
        }
        return XhsCodec.base64(json.toByteArray(Charsets.UTF_8), XhsCodec.XS_ALPHABET)
    }

    /** 分片键：`murmur3(user_id, 151488) % 100 + 1`；不知道用户 id 时随机（多位用户确认不带 / 随机也行）。 */
    fun xyDirection(userId: String?): Int =
        if (userId.isNullOrEmpty()) 10 + random.nextInt(91)
        else ((XhsCodec.murmur3(userId, 151488).toLong() and 0xFFFFFFFFL) % 100).toInt() + 1

    /** `hex16((毫秒时间戳 << 23) | 序号) + 16 位随机十六进制`，追踪用，不参与校验。 */
    private fun xrayTraceId(nowMs: Long): String {
        xraySeq = (xraySeq + 1) and 0x7fffff
        val head = ((nowMs shl 23) or xraySeq.toLong())
        return "%016x".format(head) + hexRandom(16)
    }

    private fun hexRandom(n: Int): String = buildString { repeat(n) { append("0123456789abcdef"[random.nextInt(16)]) } }

    /** b1：一次生成、之后一直用（网页把它存在 localStorage 里），所以由调用方存进 `ctx.store`。 */
    fun newB1(nowMs: Long, loadtsMs: Long): String = XhsB1.encode(XhsB1.fields(profile.b1, nowMs, loadtsMs))

    companion object {
        /** dsllt（`ds` 脚本加载时间）网页每 15 分钟刷新一次。 */
        const val DSL_REFRESH_MS = 15 * 60 * 1000L
    }
}

/** x3：`mns0301_` 这一档，稳态业务请求都用它。 */
internal object XhsMns {
    const val PREFIX = "mns0301_"

    /**
     * 144 字节明文（a1 固定 52 个字符时）：
     * ```
     * 4   魔数 79 68 60 29
     * 4   version（每次随机的 u32，低字节 v 用来异或后面几段）
     * 8   当前毫秒时间戳（小端）
     * 8   页面加载时间 loadts（小端）
     * 4   请求序号 seq（小端）
     * 4   envConst（小端）
     * 4   内容长度（UTF-8 字节数，小端）
     * 8   MD5(内容) 的前 8 字节 ⊕ v
     * 1+n a1 的长度和内容
     * 1+n appId 的长度和内容
     * 16  环境位：01, v⊕115, 然后 14 字节 envFpTail
     * 3   设备标签 "a3"：长度 2, 'a', '3'
     * 17  长度 16, 然后 16 字节 dsf(时间戳小端 8 字节 ++ MD5(api)) ⊕ v
     * ```
     * 内容 = api + 请求体；设备签名对 api（GET 带查询串）而不是内容取 MD5。
     */
    fun pack(
        version: Int,
        tsMs: Long,
        loadtsMs: Long,
        seq: Int,
        envConst: Int,
        content: String,
        api: String,
        a1: String,
        appId: String,
        envFpTail: ByteArray,
    ): ByteArray {
        require(envFpTail.size == 14) { "envFpTail 要 14 字节" }
        val v = version and 0xFF
        val contentBytes = content.toByteArray(Charsets.UTF_8)
        val md5Full = XhsCodec.md5(contentBytes)
        val md5Api = XhsCodec.md5(api)
        val tsLe = le(tsMs, 8)
        val out = java.io.ByteArrayOutputStream(144)
        out.write(byteArrayOf(121, 104, 96, 41))
        out.write(le(version.toLong() and 0xFFFFFFFFL, 4))
        out.write(tsLe)
        out.write(le(loadtsMs, 8))
        out.write(le(seq.toLong() and 0xFFFFFFFFL, 4))
        out.write(le(envConst.toLong() and 0xFFFFFFFFL, 4))
        out.write(le(contentBytes.size.toLong(), 4))
        out.write(ByteArray(8) { (md5Full[it].toInt() xor v).toByte() })
        val a1Bytes = a1.toByteArray(Charsets.UTF_8)
        out.write(a1Bytes.size)
        out.write(a1Bytes)
        val appBytes = appId.toByteArray(Charsets.UTF_8)
        out.write(appBytes.size)
        out.write(appBytes)
        out.write(1)
        out.write(v xor 115)
        out.write(envFpTail)
        out.write(byteArrayOf(2, 'a'.code.toByte(), '3'.code.toByte()))
        out.write(16)
        out.write(XhsCodec.dsf(tsLe + md5Api).let { h -> ByteArray(16) { (h[it].toInt() xor v).toByte() } })
        return out.toByteArray()
    }

    fun x3(plain: ByteArray, keystream: ByteArray): String {
        require(keystream.size >= plain.size) { "密钥流比明文短：${keystream.size} < ${plain.size}" }
        val cipher = ByteArray(plain.size) { (plain[it].toInt() xor keystream[it].toInt()).toByte() }
        return PREFIX + XhsCodec.base64(cipher, XhsCodec.X3_ALPHABET)
    }

    /** X-s 外壳。x6 / x7 不带（见 [XhsSigner]）。 */
    fun xs(signVersion: String, appId: String, platform: String, x3: String, x4: String, x5: String): String {
        val json = "{\"x0\":${XhsCodec.jsonString(signVersion)},\"x1\":${XhsCodec.jsonString(appId)}," +
            "\"x2\":${XhsCodec.jsonString(platform)},\"x3\":${XhsCodec.jsonString(x3)},\"x4\":${XhsCodec.jsonString(x4)}," +
            "\"x5\":${XhsCodec.jsonString(x5)}}"
        return "XYS_" + XhsCodec.base64(json.toByteArray(Charsets.UTF_8), XhsCodec.XS_ALPHABET)
    }

    private fun le(v: Long, n: Int): ByteArray = ByteArray(n) { (v ushr (8 * it)).toByte() }
}

/**
 * b1：X-S-Common 的 x8。19 个浏览器环境字段按固定顺序拼成紧凑 JSON，用 RC4（密钥 `xhswebmplfbt`）加密，
 * 密文当 Latin-1 字符串再按 UTF-8 编码成字节，最后自定义 Base64。
 * 字段值来自 sign_profile.json，只有 x36（帧数）、x44（生成时间）、x84（含 timeOrigin）是现算的。
 */
internal object XhsB1 {
    private const val RC4_KEY = "xhswebmplfbt"

    fun fields(p: XhsProfile.B1, nowMs: Long, loadtsMs: Long): LinkedHashMap<String, String> {
        val timeOrigin = BigDecimal.valueOf(loadtsMs.toDouble() + p.timeOriginOffsetMs).toPlainString()
        return linkedMapOf(
            "x33" to "0",
            "x34" to "0",
            "x35" to "0",
            "x36" to p.frameCount.toString(),
            "x37" to p.x37,
            "x38" to p.x38,
            "x39" to p.x39,
            "x42" to p.sdkVersion,
            "x43" to p.x43,
            "x44" to nowMs.toString(),
            "x45" to p.x45,
            "x46" to p.x46,
            "x48" to p.x48,
            "x49" to p.x49,
            "x50" to p.x50,
            "x51" to p.x51,
            "x52" to p.x52,
            "x82" to p.x82.take(300),
            "x84" to p.telemetryTemplate.replace("__TIME_ORIGIN__", timeOrigin),
        )
    }

    fun encode(fields: Map<String, String>): String {
        val json = fields.entries.joinToString(",", "{", "}") {
            XhsCodec.jsonString(it.key) + ":" + XhsCodec.jsonString(it.value)
        }
        val cipher = XhsCodec.rc4(json.toByteArray(Charsets.UTF_8), RC4_KEY.toByteArray(Charsets.US_ASCII))
        // 网页对字符串逐字符异或，得到码点 0..255 的字符串，再按 UTF-8 转成字节：>= 0x80 的字节变成两个字节
        val utf8 = String(cipher, Charsets.ISO_8859_1).toByteArray(Charsets.UTF_8)
        return XhsCodec.base64(utf8, XhsCodec.XS_ALPHABET)
    }

    /** 解开 b1 看字段（测试用）。 */
    fun decode(b1: String): String {
        val utf8 = XhsCodec.base64Decode(b1, XhsCodec.XS_ALPHABET)
        val latin = String(utf8, Charsets.UTF_8).toByteArray(Charsets.ISO_8859_1)
        return String(XhsCodec.rc4(latin, RC4_KEY.toByteArray(Charsets.US_ASCII)), Charsets.UTF_8)
    }
}
