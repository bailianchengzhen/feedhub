package feedhub.source.xiaohongshu

import java.io.ByteArrayOutputStream
import java.util.zip.CRC32
import java.util.zip.Deflater
import kotlin.random.Random

/**
 * `x-rap-param`：homefeed、feed、user_posted 等接口要带的"行为 / 环境指纹"请求头。纯函数，没有网络。
 *
 * 流程（两个参考项目一致）：
 * 1. 明文 = 16 字节明文头（`03e8` + 毫秒时间戳 8 字节 + `03e9` + xxHash32([mask]) 4 字节）+ 指纹模板（219 字节 TLV）按 mask 逐字节异或。
 *    模板里 `03ea` 是 16 字节会话密钥（每次随机），`03eb` 是 xxHash32(`//edith.xiaohongshu.com` + 路径和查询串 + 请求体)；
 * 2. gzip（level 6，头里的系统字节改成 3）；
 * 3. 补到 16 的倍数（网页补随机的 0-9a-z 字符），与 16 字节随机密钥循环异或，再逐块用 [XhsAes]（换了 S 盒的 AES-128，固定密钥）加密；
 * 4. 信封：`07 24 01 <盐长度>` + 5 个 u32 + 8 个 0 字节，后面接盐、加密后的随机密钥、密文、gzip 长度，整体 Base64。
 *
 * 参考：
 * - xhshow（MIT）`core/xrap.py`：信封、gzip 头、密码块；测试向量 `tests/test_xrap.py`（见 XhsRapTest）。
 * 已落后；
 * - cv-cat/Spider_XHS（同一作者已把这些脚本以 MIT 收进 cv-cat/catbus 的 `static/xhs/`）：`xhs_pc/js/rap.js` `rap_crypto.js`，
 *   `03e9` 是 xxHash32([mask])，明文用抓下来的模板（放在 sign_profile.json 里，改版时只换数据）。
 *   我们的 [generate] 照 Spider_XHS 这一版；[pack] 这一层两个项目一致，所以能用 xhshow 的向量校验。
 */
internal object XhsRap {
    /** `0-9a-z`：掩码、会话密钥、密钥、盐、补位都从这里取（[RustXhsParse] 也要按同样的顺序取）。 */
    internal const val ALPHABET36 = "0123456789abcdefghijklmnopqrstuvwxyz"

    /** 请求哈希的前缀：浏览器对 edith 接口用协议相对地址。 */
    const val API_PREFIX = "//edith.xiaohongshu.com"

    fun randomAscii(random: Random, n: Int): ByteArray =
        ByteArray(n) { ALPHABET36[random.nextInt(ALPHABET36.length)].code.toByte() }

    /**
     * 明文：[template] 里写入会话密钥 [sessionKey]（16 字节）和请求哈希 [requestHash]，前面接 16 字节明文头，
     * 头之后的部分整体异或 [mask]。
     */
    fun payload(template: ByteArray, tsMs: Long, requestHash: Int, mask: Int, sessionKey: ByteArray): ByteArray {
        require(template.size >= 28) { "模板太短" }
        require(sessionKey.size == 16) { "会话密钥要 16 字节" }
        // 模板固定以 03ea 00000010 <16 字节密钥> 03eb <4 字节哈希> 开头
        require(
            template[0] == 0x03.toByte() && template[1] == 0xEA.toByte() && template[5] == 0x10.toByte() &&
                template[22] == 0x03.toByte() && template[23] == 0xEB.toByte(),
        ) { "模板开头不是 03ea(16 字节) + 03eb" }
        val body = template.copyOf()
        sessionKey.copyInto(body, 6)
        putInt(body, 24, requestHash)
        val out = ByteArray(16 + body.size)
        out[0] = 0x03
        out[1] = 0xE8.toByte()
        for (i in 0 until 8) out[2 + i] = (tsMs ushr (56 - 8 * i)).toByte()
        out[10] = 0x03
        out[11] = 0xE9.toByte()
        putInt(out, 12, XhsCodec.xxh32(byteArrayOf(mask.toByte())))
        for (i in body.indices) out[16 + i] = (body[i].toInt() xor mask).toByte()
        return out
    }

    /** gzip：头 `1f 8b 08 00 <mtime 小端> 00 03`（系统字节 3，和网页一致），deflate 用 zlib 的 [level]。 */
    fun gzip(raw: ByteArray, mtime: Int, level: Int = 6): ByteArray {
        val out = ByteArrayOutputStream()
        out.write(byteArrayOf(0x1F, 0x8B.toByte(), 8, 0))
        for (i in 0 until 4) out.write(mtime ushr (8 * i))
        out.write(0)
        out.write(3)
        val deflater = Deflater(level, true)
        try {
            deflater.setInput(raw)
            deflater.finish()
            val buf = ByteArray(4096)
            while (!deflater.finished()) out.write(buf, 0, deflater.deflate(buf))
        } finally {
            deflater.end()
        }
        val crc = CRC32().also { it.update(raw) }.value.toInt()
        for (i in 0 until 4) out.write(crc ushr (8 * i))
        for (i in 0 until 4) out.write(raw.size ushr (8 * i))
        return out.toByteArray()
    }

    /**
     * 信封。[gz] 是 gzip 之后的明文，[key] 是 16 字节随机密钥（循环异或用，也加密后放进信封），[salt] 是 4~6 个 ASCII 字符，
     * [pad] 是补到 16 倍数的字节，在异或之前补（网页补随机字符）；不给就按 xhshow 的做法：异或之后最后一块补 0，[encTimeMs] 是"加密耗时"字段，[version] 是协议版本。
     */
    fun pack(gz: ByteArray, key: ByteArray, salt: ByteArray, pad: ByteArray?, encTimeMs: Int, version: Int): String {
        require(key.size == 16) { "密钥要 16 字节" }
        require(salt.size in 0..255) { "盐太长" }
        val aes = XhsAes(XhsAes.RAP_KEY.toByteArray(Charsets.US_ASCII))
        val padLen = (16 - gz.size % 16) % 16
        // 补位的字节服务端不看（它按末尾的 gzip 长度截取）。网页是先补随机字符再异或；xhshow 是异或之后最后一块补 0。
        val padded = if (pad != null) gz + pad.copyOf(padLen) else gz
        val xored = ByteArray(padded.size) { (padded[it].toInt() xor key[it % 16].toInt()).toByte() }
        val cipherBody = aes.encryptEcb(xored) + intBytes(gz.size)
        val content = salt + aes.encryptBlock(key) + intBytes(16) + cipherBody
        val header = ByteArrayOutputStream().apply {
            write(byteArrayOf(0x07, 0x24, 0x01, salt.size.toByte()))
            write(intBytes(1))
            write(intBytes(20))
            write(intBytes(cipherBody.size))
            write(intBytes(XhsCodec.xxh32(content)))
            write(intBytes(version))
            write(intBytes(encTimeMs))
            write(ByteArray(8))
        }.toByteArray()
        return java.util.Base64.getEncoder().encodeToString(header + content)
    }

    /**
     * 生成一个请求的 `x-rap-param`。[api] 是路径（GET 带查询串，和请求完全一致），[body] 是 POST 的紧凑 JSON（GET 传空串）。
     */
    fun generate(rap: XhsProfile.Rap, api: String, body: String, nowMs: Long, random: Random): String {
        val mask = ALPHABET36[random.nextInt(ALPHABET36.length)].code
        val sessionKey = randomAscii(random, 16)
        val hash = XhsCodec.xxh32((API_PREFIX + api + body).toByteArray(Charsets.UTF_8))
        val plain = payload(rap.template, nowMs, hash, mask, sessionKey)
        val gz = gzip(plain, (nowMs / 1000).toInt())
        val key = randomAscii(random, 16)
        val salt = randomAscii(random, 4 + random.nextInt(3))
        val padLen = (16 - gz.size % 16) % 16
        return pack(gz, key, salt, randomAscii(random, padLen), 60 + random.nextInt(181), rap.version)
    }

    private fun putInt(b: ByteArray, o: Int, v: Int) {
        for (i in 0 until 4) b[o + i] = (v ushr (24 - 8 * i)).toByte()
    }

    private fun intBytes(v: Int): ByteArray = ByteArray(4).also { putInt(it, 0, v) }
}
