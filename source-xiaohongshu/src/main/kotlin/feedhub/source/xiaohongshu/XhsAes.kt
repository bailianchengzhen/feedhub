package feedhub.source.xiaohongshu

/**
 * x-rap-param 用的分组密码：标准 AES-128（ECB，一次一块），只是把 S 盒换成了小红书自己的 256 字节表，密钥固定。
 * xhshow 把它叫"SM4 变体"并预展开了轮密钥，Spider_XHS 的 `rap_crypto.js` 说明它其实就是换了 S 盒的 AES-128：
 * 两边的 S 盒逐字节一致，xhshow 的第 0 轮密钥字 0x6B714931 正是 ASCII "kqI1"。这里按 AES 的密钥扩展自己算轮密钥，
 * 并在单测里和 xhshow 预展开的 10 轮密钥逐字对照（见 XhsRapTest）。
 *
 * 参考：xhshow（MIT）`core/xrap.py`（S 盒、轮密钥、`encrypt_block16` 的定值）；cv-cat/Spider_XHS（同一作者已把这些脚本以 MIT 收进 cv-cat/catbus 的 `static/xhs/`）：`xhs_pc/js/aes_encrypt.js`。
 */
internal class XhsAes(key: ByteArray, private val sbox: IntArray = SBOX) {
    init {
        require(key.size == 16) { "AES-128 密钥要 16 字节" }
    }

    /** 11 个 16 字节的轮密钥，按字节顺序（第 c 个字的 4 个字节放在 [4c, 4c+4)）。 */
    private val roundKeys: Array<IntArray> = expand(key)

    /** 第 [round] 轮密钥的 4 个大端 32 位字，测试里和 xhshow 的表对照用。 */
    fun roundKeyWords(round: Int): List<Int> = (0 until 4).map { c ->
        val rk = roundKeys[round]
        (rk[4 * c] shl 24) or (rk[4 * c + 1] shl 16) or (rk[4 * c + 2] shl 8) or rk[4 * c + 3]
    }

    private fun expand(key: ByteArray): Array<IntArray> {
        val w = Array(44) { IntArray(4) }
        for (i in 0 until 4) for (j in 0 until 4) w[i][j] = key[4 * i + j].toInt() and 0xFF
        var rcon = 1
        for (i in 4 until 44) {
            val t = w[i - 1].copyOf()
            if (i % 4 == 0) {
                val r = intArrayOf(t[1], t[2], t[3], t[0])
                for (j in 0 until 4) t[j] = sbox[r[j]]
                t[0] = t[0] xor rcon
                rcon = xtime(rcon)
            }
            for (j in 0 until 4) w[i][j] = w[i - 4][j] xor t[j]
        }
        return Array(11) { round -> IntArray(16) { w[4 * round + it / 4][it % 4] } }
    }

    private fun xtime(a: Int): Int = ((a shl 1) xor (if (a and 0x80 != 0) 0x1B else 0)) and 0xFF

    private fun mul2(a: Int) = xtime(a)
    private fun mul3(a: Int) = xtime(a) xor a

    /** 加密一个 16 字节的块；不足 16 字节的右边补 0。 */
    fun encryptBlock(block: ByteArray): ByteArray {
        require(block.size <= 16) { "块长不能超过 16" }
        val s = IntArray(16) { if (it < block.size) block[it].toInt() and 0xFF else 0 }
        addRoundKey(s, 0)
        for (round in 1..10) {
            for (i in 0 until 16) s[i] = sbox[s[i]]
            shiftRows(s)
            if (round < 10) mixColumns(s)
            addRoundKey(s, round)
        }
        return ByteArray(16) { s[it].toByte() }
    }

    private fun addRoundKey(s: IntArray, round: Int) {
        val rk = roundKeys[round]
        for (i in 0 until 16) s[i] = s[i] xor rk[i]
    }

    private fun shiftRows(s: IntArray) {
        val t = s.copyOf()
        for (c in 0 until 4) for (r in 0 until 4) s[4 * c + r] = t[4 * ((c + r) % 4) + r]
    }

    private fun mixColumns(s: IntArray) {
        for (c in 0 until 4) {
            val a0 = s[4 * c]
            val a1 = s[4 * c + 1]
            val a2 = s[4 * c + 2]
            val a3 = s[4 * c + 3]
            s[4 * c] = mul2(a0) xor mul3(a1) xor a2 xor a3
            s[4 * c + 1] = a0 xor mul2(a1) xor mul3(a2) xor a3
            s[4 * c + 2] = a0 xor a1 xor mul2(a2) xor mul3(a3)
            s[4 * c + 3] = mul3(a0) xor a1 xor a2 xor mul2(a3)
        }
    }

    /** ECB：逐块加密，最后一块不足 16 字节就用 [pad]（长度够补满）补，默认补 0。 */
    fun encryptEcb(data: ByteArray, pad: ByteArray? = null): ByteArray {
        val full = if (data.size % 16 == 0) data else data + (pad ?: ByteArray(16 - data.size % 16)).copyOf(16 - data.size % 16)
        val out = ByteArray(full.size)
        for (o in full.indices step 16) encryptBlock(full.copyOfRange(o, o + 16)).copyInto(out, o)
        return out
    }

    companion object {
        /** 小红书自己的 S 盒（xhshow 和 Spider_XHS 逐字节一致）。 */
        val SBOX: IntArray = (
        "7a0158e0504e02791d4b53da6b48d452ed7712211415ec1018e5b9f10c08fc7d" +
        "f9cdb5c8e637268756bab82badf068f78b8dd35e364d2e923182f229703d2dd7" +
        "b640b243448078d20d494a09636c073a9ed506c6e162f4342459a9572a003e17" +
        "2c0a1a42fa93bedcf5b36a13e803c797bb737686e3467247d0054c387c1f81ab" +
        "7551ebf33274118f84899c71227e9dcf3f9169653c6d96a2989933399acac39f" +
        "a0bce4a3a4547fa7a8046f5dacb727afb02841aeb46e0b1bdf8e30b1fe906160" +
        "c0cb5c0eef1683ea20e9c955c44585cc1eaa678a7b35d619d8d9c2db94dd1cde" +
        "a6fff8bf5b5a0fe7c1bdd166c525ee8ce25f88a13ba5f6ce952f6423fbfd4f9b" +
            ""
            ).chunked(2).filter { it.isNotEmpty() }.map { it.toInt(16) }.toIntArray()

        /** x-rap-param 的固定密钥。 */
        const val RAP_KEY = "kqI1DTcwKX90ZtAy"
    }
}
