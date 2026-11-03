package feedhub.source.tiktok

import java.io.ByteArrayOutputStream
import java.security.MessageDigest
import java.util.Base64

/**
 * TikTok 网页版的 HTTP 签名：`X-Dynosaur` + `X-Gnarly`（外加固定的 `X-Bogus=1`）。
 *
 * ## 来源与许可
 *
 * 逐行移植自
 * `Johnserf-Seed/f2: f2/utils/crypto/bytedance/xgnarly.py`
 * （作者 JohnserfSeed，**Apache License 2.0**；按 License 在此写明来源）。
 * 对应 webmssdk `2.0.0.561` / 载荷版本 `5.3.2`。
 *
 * 那一份的权威性来自它自己的核对方式：作者用 Node 在无界面环境里跑**真实 SDK**，用 `unseal` 解出
 * SDK 生成的签名，读出里面的时间戳与随机数，代回自己的实现重算后逐字段比对（4 个接口 × 3 种 UA ×
 * 20 组向量）。仓库里**固化了 3 组**带期望值的向量（`tests/test_xgnarly.py`），本模块的
 * `TiktokSignTest` 就是照那 3 组移植的。
 *
 * ## 算法要点（改之前先读）
 *
 * - 两个签名共用一种"信封"：`payload` 是 TLV 字段表 → 12 个随机 uint32 当密钥 → 一轮**改过的
 *   ChaCha**（常量不是 `expand 32-byte k`、轮数 `5 + (sum(key & 0xF) & 0xF)`、奇数轮在列轮之后退出、
 *   对角轮第三组是 `(2,7,12,13)` 而不是标准的 `(2,7,8,13)`——原实现的笔误，**要刻意保持一致**）→
 *   把 48 字节密钥按 `(sum(密钥字节) + sum(密文字节)) % (len(密文)+1)` 插进密文 →
 *   `0x4B` 开头，标准 base64 再按自定义字母表换字。
 * - `X-Dynosaur` 的 `0x2E` 是**业务查询串**（只有查询串、不含路径）的 FNV 变体 hash_state，
 *   `0x30` 是 User-Agent 的，`0x2B` 是空 body 的。**签名用的 UA 必须和请求头里的 UA 逐字节一致**。
 * - `X-Gnarly` 封的是**追加了 `X-Dynosaur` 与 `msToken` 之后**的查询串，所以
 *   `X-Bogus=1` 夹在它们之间、不在封印范围内（平台自己的不对称设计）。
 * - `msToken` **原样使用，空字符串也一样，绝不伪造**：f2 的实测——
 *   真令牌 2545 字节、不带令牌 2541 字节、伪造的同长度令牌 0 字节 + `tt_orcas_res: 1`。
 * - 整个载荷里服务器只校验 `envcode`（65）与 `ubcode`（8）两个值，写错**没有任何提示**：
 *   除 `/api/post/item_list/` 之外的接口照常响应，`item_list` 类接口返回 200 + 空 body +
 *   `tt_orcas_res: 1`（和"被 TLS 指纹拦了"看起来一模一样）。这两个值要以**浏览器抓包**为准
 *
 * ## 编码口径（两个签名共用的唯一入口）
 *
 * 业务参数按 **RFC 3986**（只保留 `A-Za-z0-9-_.~`，其余一律 `%XX`）编码后拼成查询串，
 * 签名（`hash_state` / `md5`）覆盖的就是这串字节。这一点有两个独立来源：f2 的
 * `XGnarlyManager.model_2_endpoint`与 TiktokApis 的 `Params.to_query`
 * （业务参数 `quote(value, safe="-_.~")`）。
 *
 * 签名参数本身（`X-Dynosaur` / `msToken` / `X-Bogus` / `X-Gnarly`）**原样拼接、不再编码**：
 * 字母表里有 `/`、填充是 `=`，TikTok 网页发送时两者都不转义。
 *
 * [encodeQuery] / [escape] 是 f2 的**低层**编码（只转义浏览器必须转义的字符），只用来复现它那 3 组
 * 对拍向量；生产路径用 [rfc3986Query]。
 */
internal object TiktokSign {
    const val MASK32 = 0xFFFFFFFFL

    /** 四个签名参数在查询串里的顺序（平台固定：业务参数 → Dynosaur → msToken → Bogus → Gnarly）。 */
    const val DYNOSAUR_PARAM = "X-Dynosaur"
    const val MS_TOKEN_PARAM = "msToken"
    const val BOGUS_PARAM = "X-Bogus"
    const val GNARLY_PARAM = "X-Gnarly"

    /** HTTP 请求里 `X-Bogus` 固定是字符串 `"1"`：完整算出来的那套只用在 WebSocket 握手里。 */
    const val BOGUS_VALUE = "1"

    /**
     * 自定义 base64 字母表（64 字符，标准字母表按位置一一对应）。两个签名都用它；
     * 字符串表里另一张字母表属于 X-Bogus，这里用不到。
     */
    const val ALPHABET = "u09tbS3UvgDEe6r-ZVMXzLpsAohTn7mdINQlW412GqBjfYiyk8JORCF5/xKHwacP"
    private const val STANDARD = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/"

    /** 信封的第一个字节，位于插入了密钥的密文之前。 */
    const val ENVELOPE_TAG = 0x4B

    /** ChaCha 状态的第 0～3 个字：不是标准的 `expand 32-byte k`，是 SDK 里的值。 */
    val CHACHA_INIT = longArrayOf(1196819126, 600974999, 3863347763, 1451689750)

    /** FNV-1a 32 位变体：偏移基数非标准，且每个字节额外乘 33。 */
    const val FNV_OFFSET = 2166136260L
    const val FNV_PRIME = 16777619L

    /** 载荷里携带的版本号（SDK 2.0.0.561）。**SDK 升级后这两个值和字段表都可能变**。 */
    const val SDK_VERSION = "5.3.2"
    const val SCM_VERSION = "2.0.0.561"

    /** 环境指纹：只有这两个值会被服务器校验（见类说明）。 */
    const val ENV_CODE = 65
    const val UB_CODE = 8

    /** `X-Dynosaur` 的 `0x38`：SDK 内部某个 md5 的 hash_state，与环境无关，直接用观测值。 */
    const val VM_STATE_HASH = 0xC46CE353L

    /** canvas 指纹：`"-1"` = 没有 2d 上下文的页面上报的值。 */
    const val CANVAS_HASH = "-1"

    /**
     * 另外三个模拟环境生成不了的字段（WebGL hash / 组件版本 / device hash），暂时固定 `"0"`。
     * 真实浏览器在 `0x28` 报一个 32 位哈希、`0x32` 报组件版本串、`0x33` 报一个 md5；
     * 平台日后再开始校验时这里要换成每个身份固定的值。
     */
    const val WEBGL_HASH = "0"
    const val COMPONENT_VERSION = "0"
    const val DEVICE_HASH = "0"

    /** SDK 认为自己所在页面的 `location.host + location.pathname`：没有页面的客户端如实报首页。 */
    const val PAGE = "www.tiktok.com/"

    /** SDK 从 1 开始统计自己的签名次数；每次请求都是新身份的客户端就报 1。 */
    const val CALL_SEQUENCE_START = 1

    /** 空请求体的 md5（GET 都是这个值）。 */
    const val EMPTY_BODY_MD5 = "d41d8cd98f00b204e9800998ecf8427e"

    /** 签名信封里插入的密钥字数。 */
    const val KEY_WORDS = 12

    // SDK 的两种字节编码参数 (xor_base, add_base, pre_xor, rot, post_add)：A 用于除校验和以外的字段，
    // B 用于校验和与三个标志位。
    private val ENCODER_A = longArrayOf(103, 1, -1, 2, 1)
    private val ENCODER_B = longArrayOf(102, 0, 165, 1, 0)

    /** 浏览器在查询串里只会转义这几个字符（f2 的 `_MUST_ESCAPE`，只用于复现它的对拍向量）。 */
    private val MUST_ESCAPE = mapOf(
        " " to "%20", "\"" to "%22", "<" to "%3C", ">" to "%3E", "`" to "%60", "#" to "%23",
    )

    private val TO_CUSTOM: IntArray = IntArray(128).also { table ->
        for (i in 0 until 128) table[i] = i
        for (i in STANDARD.indices) table[STANDARD[i].code] = ALPHABET[i].code
    }
    private val FROM_CUSTOM: IntArray = IntArray(128).also { table ->
        for (i in 0 until 128) table[i] = i
        for (i in ALPHABET.indices) table[ALPHABET[i].code] = STANDARD[i].code
    }

    /** 一次签名结果：要发送的查询串 + 四个签名参数。 */
    class Signed(val query: String, val params: Map<String, String>) {
        /** 业务查询串（不含四个签名参数）。 */
        val businessQuery: String get() = query.substringBefore("&$DYNOSAUR_PARAM=")
    }

    // ---------- 查询串编码 ----------

    /** f2 的 `_escape`：只转义浏览器必须转义的字符，其余（括号、斜杠、冒号、逗号）保持原样。 */
    fun escape(text: String): String = buildString {
        for (ch in text) {
            val mapped = MUST_ESCAPE[ch.toString()]
            when {
                mapped != null -> append(mapped)
                ch > ' ' && ch <= '~' -> append(ch)
                else -> for (b in ch.toString().toByteArray(Charsets.UTF_8)) {
                    append('%').append(HEX[(b.toInt() shr 4) and 0xF]).append(HEX[b.toInt() and 0xF])
                }
            }
        }
    }

    /** f2 的 `encode_query`：低层编码 + `&` 拼接。**只用于复现它的对拍向量**。 */
    fun encodeQuery(pairs: List<Pair<String, String>>): String =
        pairs.joinToString("&") { (k, v) -> "${escape(k)}=${escape(v)}" }

    /**
     * 生产路径用的编码：只保留 `A-Za-z0-9-_.~`，其余一律 `%XX`（RFC 3986）。
     * 出处见类说明（f2 的 `quote(value, safe="")` 与 TiktokApis 的 `quote(value, safe="-_.~")`）。
     */
    fun rfc3986(value: String): String = buildString {
        for (b in value.toByteArray(Charsets.UTF_8)) {
            val c = b.toInt() and 0xFF
            val ch = c.toChar()
            if (ch in 'A'..'Z' || ch in 'a'..'z' || ch in '0'..'9' || ch == '-' || ch == '_' || ch == '.' || ch == '~') {
                append(ch)
            } else {
                append('%').append(HEX[(c shr 4) and 0xF]).append(HEX[c and 0xF])
            }
        }
    }

    /** 生产路径的查询串：业务参数按 [rfc3986] 编码，顺序照调用方给的（顺序是签名输入的一部分）。 */
    fun rfc3986Query(pairs: List<Pair<String, String>>): String =
        pairs.joinToString("&") { (k, v) -> "$k=${rfc3986(v)}" }

    // ---------- 原语 ----------

    /** SDK 的 FNV-1a 变体：标准的一轮之后再乘 33。 */
    fun hashState(text: String): Long {
        var value = FNV_OFFSET
        for (b in text.toByteArray(Charsets.UTF_8)) {
            val step = ((value xor (b.toLong() and 0xFF)) * FNV_PRIME) and MASK32
            value = (step + ((step * 32) and MASK32)) and MASK32
        }
        return value
    }

    /** 编码一个载荷字段：逐位置打乱字节、填充并带上长度（至少 6 字节，以 `0x00` + 原始长度结尾）。 */
    private fun encodeBytes(text: String, config: LongArray): ByteArray {
        val xorBase = config[0]
        val addBase = config[1]
        val preXor = config[2]
        val rotate = config[3].toInt()
        val postAdd = config[4]
        val size = maxOf(text.length + 2, 6)
        val out = ByteArray(size)
        for (index in text.indices) {
            var v = (text[index].code.toLong() xor (xorBase + index)) and MASK32
            v = (v + addBase + (170 and index)) % 256
            if (preXor >= 0) v = v xor preXor
            v = ((v shl rotate) or (v shr (8 - rotate))) and 0xFF
            out[index] = (((v xor 187) + postAdd) % 256).toByte()
        }
        for (index in text.length until size - 2) out[index] = ((221 + index) and 0xFF).toByte()
        out[size - 2] = 0
        out[size - 1] = (text.length and 0xFF).toByte()
        return out
    }

    /** 把字段排成平台解析的 TLV 条目（`[key][0][len][value…]`）；[leadCount] 时前面加一字节字段数。 */
    fun packPayload(fields: Map<Int, ByteArray>, order: List<Int>? = null, leadCount: Boolean = false): ByteArray {
        val keys = order ?: fields.keys.sorted()
        val out = ByteArrayOutputStream()
        if (leadCount) out.write(keys.size and 0xFF)
        for (key in keys) {
            val value = fields.getValue(key)
            out.write(key and 0xFF)
            out.write(0)
            out.write(value.size and 0xFF)
            out.write(value)
        }
        return out.toByteArray()
    }

    /** [packPayload] 的逆操作（测试 / 核对用）。 */
    fun unpackPayload(payload: ByteArray, leadCount: Boolean = false): Map<Int, ByteArray> {
        val fields = LinkedHashMap<Int, ByteArray>()
        var offset = if (leadCount) 1 else 0
        while (offset + 3 <= payload.size) {
            val key = payload[offset].toInt() and 0xFF
            val length = payload[offset + 2].toInt() and 0xFF
            fields[key] = payload.copyOfRange(offset + 3, minOf(offset + 3 + length, payload.size))
            offset += 3 + length
        }
        return fields
    }

    /** [encodeBytes] 的逆操作（测试 / 核对用）。 */
    fun decodeField(raw: ByteArray, variant: Char): String {
        val config = if (variant == 'a') ENCODER_A else ENCODER_B
        val xorBase = config[0]
        val addBase = config[1]
        val preXor = config[2]
        val rotate = config[3].toInt()
        val postAdd = config[4]
        val out = StringBuilder()
        for (index in 0 until (raw.last().toInt() and 0xFF)) {
            var v = (((raw[index].toLong() and 0xFF) - postAdd) % 256 + 256) % 256
            v = v xor 187
            v = ((v shr rotate) or (v shl (8 - rotate))) and 0xFF
            if (preXor >= 0) v = v xor preXor
            v = ((v - addBase - (170 and index)) % 256 + 256) % 256
            out.append(((v xor (xorBase + index)) and 0xFF).toInt().toChar())
        }
        return out.toString()
    }

    private fun be(value: Long, size: Int): ByteArray {
        val out = ByteArray(size)
        for (i in 0 until size) out[i] = ((value shr (8 * (size - 1 - i))) and 0xFF).toByte()
        return out
    }

    /** 把两个 32 位值折叠为 16 位，再在高位写入环境码。 */
    private fun mix(timestamp: Long, nonce: Long, envCode: Int): Long {
        val folded = ((timestamp shr 16) xor (nonce shr 16) xor timestamp xor nonce) and 0xFFFF
        return folded or (envCode.toLong() shl 16)
    }

    /** 对字段值做异或折叠，初值为全 1（所以输出的两个校验和是普通折叠结果的补码）。 */
    private fun checksum(values: List<Any>, mode: Int): Long {
        var accumulator = MASK32
        for (value in values) {
            val number = when (value) {
                is String -> if (mode == 1) 0L else {
                    val bytes = value.toByteArray(Charsets.UTF_8)
                    var n = 0L
                    for (i in 0 until minOf(4, bytes.size)) n = (n shl 8) or (bytes[i].toLong() and 0xFF)
                    n
                }
                is Long -> value
                else -> (value as Int).toLong()
            }
            accumulator = accumulator xor (number and MASK32)
        }
        return accumulator and MASK32
    }

    private fun rotl(value: Long, count: Int): Long = ((value shl count) or (value shr (32 - count))) and MASK32

    private fun quarterRound(state: LongArray, a: Int, b: Int, c: Int, d: Int) {
        state[a] = (state[a] + state[b]) and MASK32
        state[d] = rotl(state[d] xor state[a], 16)
        state[c] = (state[c] + state[d]) and MASK32
        state[b] = rotl(state[b] xor state[c], 12)
        state[a] = (state[a] + state[b]) and MASK32
        state[d] = rotl(state[d] xor state[a], 8)
        state[c] = (state[c] + state[d]) and MASK32
        state[b] = rotl(state[b] xor state[c], 7)
    }

    /** 生成一个 64 字节的密钥流块。**这不是 ChaCha20**，差异见类说明。 */
    private fun keystream(state: LongArray, rounds: Int): LongArray {
        val working = state.copyOf()
        var done = 0
        while (done < rounds) {
            quarterRound(working, 0, 4, 8, 12)
            quarterRound(working, 1, 5, 9, 13)
            quarterRound(working, 2, 6, 10, 14)
            quarterRound(working, 3, 7, 11, 15)
            done++
            if (done >= rounds) break
            quarterRound(working, 0, 5, 10, 15)
            quarterRound(working, 1, 6, 11, 12)
            quarterRound(working, 2, 7, 12, 13)
            quarterRound(working, 3, 4, 13, 14)
            done++
        }
        return LongArray(16) { (working[it] + state[it]) and MASK32 }
    }

    /** 把载荷按小端 uint32 读出来，与密钥流异或。 */
    private fun crypt(key: LongArray, rounds: Int, payload: ByteArray): ByteArray {
        val state = LongArray(16)
        CHACHA_INIT.copyInto(state)
        for (i in 0 until KEY_WORDS) state[4 + i] = key[i] and MASK32
        val length = payload.size
        val wordCount = (length + 3) / 4
        val words = LongArray(wordCount)
        for (i in 0 until wordCount) {
            var w = 0L
            for (j in 3 downTo 0) {
                val index = 4 * i + j
                w = (w shl 8) or (if (index < length) payload[index].toLong() and 0xFF else 0L)
            }
            words[i] = w
        }
        var offset = 0
        while (offset + 16 < wordCount) {
            val block = keystream(state, rounds)
            state[12] = (state[12] + 1) and MASK32
            for (i in 0 until 16) words[offset + i] = words[offset + i] xor block[i]
            offset += 16
        }
        val block = keystream(state, rounds)
        for (i in 0 until wordCount - offset) words[offset + i] = words[offset + i] xor block[i]
        val out = ByteArray(length)
        for (i in 0 until wordCount) {
            for (j in 0 until 4) {
                val index = 4 * i + j
                if (index < length) out[index] = ((words[i] shr (8 * j)) and 0xFF).toByte()
            }
        }
        return out
    }

    /** 加密、把密钥插回密文并编码。 */
    fun seal(payload: ByteArray, key: LongArray): String {
        val rounds = ((key.sumOf { it and 0xF }) and 0xF).toInt() + 5
        val ciphertext = crypt(key, rounds, payload)
        val keyBytes = ByteArray(KEY_WORDS * 4)
        for (i in 0 until KEY_WORDS) {
            for (j in 0 until 4) keyBytes[4 * i + j] = ((key[i] shr (8 * j)) and 0xFF).toByte()
        }
        val sum = keyBytes.sumOf { it.toInt() and 0xFF } + ciphertext.sumOf { it.toInt() and 0xFF }
        val position = sum % (ciphertext.size + 1)
        val raw = ByteArray(1 + ciphertext.size + keyBytes.size)
        raw[0] = ENVELOPE_TAG.toByte()
        ciphertext.copyInto(raw, 1, 0, position)
        keyBytes.copyInto(raw, 1 + position)
        ciphertext.copyInto(raw, 1 + position + keyBytes.size, position, ciphertext.size)
        return toCustomBase64(raw)
    }

    /** 从签名里还原 (载荷, 密钥)。**核对本模块的唯一可靠依据**：拿浏览器抓到的签名解开逐字段比对。 */
    fun unseal(token: String): Pair<ByteArray, LongArray> {
        val raw = Base64.getDecoder().decode(fromCustomBase64(token))
        require(raw.isNotEmpty() && (raw[0].toInt() and 0xFF) == ENVELOPE_TAG) { "不是有效的签名信封" }
        val spliced = raw.copyOfRange(1, raw.size)
        val width = KEY_WORDS * 4
        for (position in 0..(spliced.size - width)) {
            val keyBytes = spliced.copyOfRange(position, position + width)
            val ciphertext = spliced.copyOfRange(0, position) + spliced.copyOfRange(position + width, spliced.size)
            val sum = keyBytes.sumOf { it.toInt() and 0xFF } + ciphertext.sumOf { it.toInt() and 0xFF }
            if (position != sum % (ciphertext.size + 1)) continue
            val key = LongArray(KEY_WORDS) { i ->
                var w = 0L
                for (j in 3 downTo 0) w = (w shl 8) or (keyBytes[4 * i + j].toLong() and 0xFF)
                w
            }
            val rounds = ((key.sumOf { it and 0xF }) and 0xF).toInt() + 5
            return crypt(key, rounds, ciphertext) to key
        }
        throw IllegalArgumentException("找不到满足密钥关系的插入位置")
    }

    private fun toCustomBase64(raw: ByteArray): String =
        Base64.getEncoder().encodeToString(raw).map { ch ->
            if (ch.code < 128) TO_CUSTOM[ch.code].toChar() else ch
        }.joinToString("")

    private fun fromCustomBase64(token: String): String =
        token.map { ch -> if (ch.code < 128) FROM_CUSTOM[ch.code].toChar() else ch }.joinToString("")

    // ---------- 载荷 ----------

    /**
     * 环境报告，共 25 个 TLV 字段（键 `0x20`～`0x38`）。其中三个 hash_state 是报告与请求绑定的仅有依据：
     * `0x2B` 空字符串、`0x2E` 查询串、`0x30` User-Agent。
     */
    fun dynosaurPayload(
        query: String,
        userAgent: String,
        timestamp: Long,
        nonce: Long,
        sequence: Int = CALL_SEQUENCE_START,
        envCode: Int = ENV_CODE,
        ubCode: Int = UB_CODE,
    ): ByteArray {
        val fields = LinkedHashMap<Int, ByteArray>()
        val a = { text: String -> encodeBytes(text, ENCODER_A) }
        fields[0x21] = encodeBytes("1", ENCODER_B)
        fields[0x22] = encodeBytes("1", ENCODER_B)
        fields[0x23] = a("0")
        fields[0x24] = a(mix(timestamp, nonce, envCode).toString())
        fields[0x25] = a(sequence.toString())
        fields[0x26] = a(envCode.toString())
        fields[0x27] = a(timestamp.toString())
        fields[0x28] = a(WEBGL_HASH)
        fields[0x29] = a("0")
        fields[0x2A] = a(SDK_VERSION)
        fields[0x2B] = be(hashState(""), 4)
        fields[0x2C] = a(CANVAS_HASH)
        fields[0x2D] = a("0")
        fields[0x2E] = be(hashState(query), 4)
        fields[0x2F] = a(sequence.toString())
        fields[0x30] = be(hashState(userAgent), 4)
        fields[0x31] = a(SCM_VERSION)
        fields[0x32] = a(COMPONENT_VERSION)
        fields[0x33] = a(DEVICE_HASH)
        fields[0x34] = a(nonce.toString())
        fields[0x35] = a(PAGE)
        fields[0x36] = a(ubCode.toString())
        fields[0x37] = a("0")
        fields[0x38] = be(VM_STATE_HASH, 4)
        // 先占位：任何单字符字符串都行（单字符字段的第 1 个字节是固定填充字节），SDK 用的是 "0"
        fields[0x20] = a("0")
        var checksum = 0L
        for (key in fields.keys.sorted()) checksum = checksum xor (fields.getValue(key)[1].toLong() and 0xFF)
        fields[0x20] = encodeBytes(checksum.toString(), ENCODER_B)
        return packPayload(fields)
    }

    /**
     * 请求封印的字段表，共 16 项（**没有 `0x07`**）。[signedQuery] 是**已经追加了
     * `&X-Dynosaur=…&msToken=…`** 的业务查询串：即使 msToken 为空也必须带上这段后缀。
     */
    fun gnarlyFields(
        signedQuery: String,
        userAgent: String,
        body: ByteArray = ByteArray(0),
        timestamp: Long,
        nonce: Long,
        nonce2: Long,
        sequence: Int = CALL_SEQUENCE_START,
        envCode: Int = ENV_CODE,
        ubCode: Int = UB_CODE,
    ): Map<Int, ByteArray> {
        val queryMd5 = md5Hex(signedQuery)
        val bodyMd5 = md5Hex(body)
        val agentMd5 = md5Hex(userAgent)
        val mixed = mix(timestamp, nonce, envCode)
        val covered: List<Any> = listOf(
            0L, envCode.toLong(), ubCode.toLong(), queryMd5, bodyMd5, agentMd5, timestamp, 0L,
            nonce, SDK_VERSION, SCM_VERSION, CALL_SEQUENCE_START.toLong(), sequence.toLong(),
            sequence.toLong(), mixed, nonce2,
        )
        val first = checksum(covered, 2)
        val second = checksum(covered + first, 1)
        return linkedMapOf(
            0x00 to be(second, 4),
            0x01 to be(envCode.toLong(), 2),
            0x02 to be(ubCode.toLong(), 2),
            0x03 to queryMd5.toByteArray(Charsets.US_ASCII),
            0x04 to bodyMd5.toByteArray(Charsets.US_ASCII),
            0x05 to agentMd5.toByteArray(Charsets.US_ASCII),
            0x06 to be(timestamp, 4),
            0x08 to be(nonce, 4),
            0x09 to SDK_VERSION.toByteArray(Charsets.US_ASCII),
            0x0A to SCM_VERSION.toByteArray(Charsets.US_ASCII),
            0x0B to be(CALL_SEQUENCE_START.toLong(), 2),
            0x0C to be(sequence.toLong(), 2),
            0x0D to be(sequence.toLong(), 2),
            0x0E to be(mixed, 4),
            0x0F to be(nonce2, 4),
            0x10 to be(first, 4),
        )
    }

    /** [gnarlyFields] 的字段表，打包在一字节的字段数之后。 */
    fun gnarlyPayload(
        signedQuery: String,
        userAgent: String,
        body: ByteArray = ByteArray(0),
        timestamp: Long,
        nonce: Long,
        nonce2: Long,
        sequence: Int = CALL_SEQUENCE_START,
        envCode: Int = ENV_CODE,
        ubCode: Int = UB_CODE,
    ): ByteArray = packPayload(
        gnarlyFields(signedQuery, userAgent, body, timestamp, nonce, nonce2, sequence, envCode, ubCode),
        leadCount = true,
    )

    fun md5Hex(value: String): String = md5Hex(value.toByteArray(Charsets.UTF_8))

    fun md5Hex(value: ByteArray): String =
        MessageDigest.getInstance("MD5").digest(value).joinToString("") { "%02x".format(it) }

    /**
     * 为一个请求签名。[query] 是**已经编码好**的业务查询串（不含四个签名参数）——
     * 两个签名覆盖的就是这串字节，**签名后不能再重新编码**。
     *
     * @return [Signed.query] 是要原样发送的查询串；[Signed.params] 是四个签名参数。
     */
    fun sign(
        query: String,
        userAgent: String,
        msToken: String = "",
        body: ByteArray = ByteArray(0),
        timestamp: Long,
        nonce: Long,
        nonce2: Long,
        sequence: Int = CALL_SEQUENCE_START,
        key: LongArray,
        key2: LongArray,
        envCode: Int = ENV_CODE,
        ubCode: Int = UB_CODE,
    ): Signed {
        val dynosaur = seal(
            dynosaurPayload(query, userAgent, timestamp, nonce, sequence, envCode, ubCode),
            key,
        )
        // 封印覆盖追加了 X-Dynosaur 与 msToken 之后的查询串，所以先拼接再算 X-Gnarly
        val sealedQuery = "$query&$DYNOSAUR_PARAM=$dynosaur&$MS_TOKEN_PARAM=$msToken"
        val gnarly = seal(
            gnarlyPayload(sealedQuery, userAgent, body, timestamp, nonce, nonce2, sequence, envCode, ubCode),
            key2,
        )
        val params = linkedMapOf(
            DYNOSAUR_PARAM to dynosaur,
            MS_TOKEN_PARAM to msToken,
            BOGUS_PARAM to BOGUS_VALUE,
            GNARLY_PARAM to gnarly,
        )
        // 原样拼接，不做百分号编码：字母表里有 /，填充是 =，TikTok 网页发送时两者都不转义
        val signed = "$sealedQuery&$BOGUS_PARAM=$BOGUS_VALUE&$GNARLY_PARAM=$gnarly"
        return Signed(signed, params)
    }

    private val HEX = "0123456789ABCDEF".toCharArray()

    /** f2 的 `_clock_nonce`：由微秒时钟得到的 32 位值（这里用毫秒时钟 ×1000）。 */
    fun clockNonce(clockMs: () -> Long = System::currentTimeMillis): Long = (clockMs() * 1000) and MASK32
}

/**
 * 签名用的随机源（12 个密钥字 + 两个 nonce）。生产用 [SYSTEM]，测试里换成常量以便复现。
 * f2 里两个 nonce 都来自微秒时钟（`_clock_nonce`），密钥来自 `random.getrandbits(32)`。
 */
internal fun interface TiktokRandom {
    /** 下一个 32 位无符号值（`0..0xFFFFFFFF`）。 */
    fun nextWord(): Long

    companion object {
        val SYSTEM = TiktokRandom { java.util.concurrent.ThreadLocalRandom.current().nextLong() ushr 32 }

        /** 所有随机值都由这个数列给（测试用；固定种子 → 固定签名）。 */
        fun of(seed: Long) = object : TiktokRandom {
            private val rng = java.util.Random(seed)
            override fun nextWord(): Long = rng.nextLong() ushr 32
        }
    }
}
