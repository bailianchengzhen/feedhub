package feedhub.source.douyin

import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json

/**
 * a_bogus 签名要用到的、**按子域 / bdms 版本 / 会话**变的常量。
 *
 * ## 已经按 www.douyin.com 抓包定过一次
 *
 * 值在 `src/main/resources/feedhub/source/douyin/abogus_profile.json`，里面记着 `capturedFrom`
 * 和还**会变**的项。定值的做法：抓 13 条页面自己发出的 URL，用
 * `./gradlew :source-douyin:abogusProbe -Pargs="capture <抓包.json>"` 反解 —— 13/13 能反解，
 * 10/13「反解 → 重签」逐字节一致，这组值就是那些一致的值。
 *
 * 这些值**不是永久常量**：同一份 www 抓包，早先那次反解出 `u17=8`、`u37=[64,2,2,0,1]`，
 * 这次是 `u17=14`、`u37=[0,0,0,0,1]`。所以 `ctx.store` 里能放一份新的整份 JSON 覆盖（[KEY]），
 * 不用重新打包。
 *
 * | 字段 | 现值 | 备注 |
 * | [pageId] | `6241` | 13 条一致；
 * 换 UA 必须重解 |
 * | [u17] | `14` | **会变**：更早的 mafqla www 抓包是 `8` |
 * | [u37] | `[0,0,0,0,1]` | 13 条一致；更早的抓包见过 `[64,2,2,0,1]` / `[40,2,2,0,1]` / `[33,3,3,0,1]` |
 * | [counter] | `6` | `U[27]`；cv-cat 按调用次数在 3/4/5/6 之间取 |
 *
 * UA 和窗口指纹**要成套**：签名里用的 UA 必须和请求头里发的 UA 一致（`U[56]/U[57]/U[59]` 是
 * `SM3(s3-base64(RC4(UA)))` 的字节），窗口指纹串的平台段也要和 UA 的平台一致 —— 这套值是
 * [DouyinDevice] 提供的，改一处就要一起改。
 */
@Serializable
internal data class AbogusProfile(
    /** 站点 aid。www / live / login 都是 6383，creator 是 2906。 */
    val aid: Int = 6383,
    /** 页面 ID，**每个子域不同**。 */
    val pageId: Int = 6241,
    /** bdms 版本号。 */
    val sdkVersion: String = "1.0.1.19-fix.01",
    /** UA 那条 RC4 的 key 第三个字节，**按 UA / bdms 版本**解（换 UA 就必须重解）。 */
    val sdkMinor: Int = 14,
    /** `U[44]`。实测**会变**：这份抓包是 14，更早的 mafqla 抓包是 8。 */
    val u17: Int = 14,
    /** `U[37]`：前三个对应 cv-cat 的 `l40/l41/l42`。实测**会变**，见 [AbogusProfile] 的 KDoc。 */
    val u37: IntArray = intArrayOf(0, 0, 0, 0, 1),
    /** 调用计数器 `U[27]`，cv-cat 按调用次数在 3/4/5/6 之间取；这份抓包是 6。 */
    val counter: Int = 6,
) {
    // data class 带 IntArray 必须自己实现 equals/hashCode，否则比较的是引用。
    override fun equals(other: Any?): Boolean = other is AbogusProfile &&
        aid == other.aid && pageId == other.pageId && sdkVersion == other.sdkVersion &&
        sdkMinor == other.sdkMinor && u17 == other.u17 && counter == other.counter &&
        u37.contentEquals(other.u37)

    override fun hashCode(): Int {
        var result = aid
        result = 31 * result + pageId
        result = 31 * result + sdkVersion.hashCode()
        result = 31 * result + sdkMinor
        result = 31 * result + u17
        result = 31 * result + u37.contentHashCode()
        result = 31 * result + counter
        return result
    }

    companion object {
        /** 存在 `ctx.store` 里的覆盖版本（整份 JSON）。 */
        const val KEY = "douyin.abogus_profile"

        private val json = Json { ignoreUnknownKeys = true }

        fun parse(text: String): AbogusProfile = json.decodeFromString(serializer(), text)

        /**
         * 随包的版本：`resources/feedhub/source/douyin/abogus_profile.json`。
         * 拿不到资源时退回 hardcode 的默认值（测试环境的 resources 目录偶尔会缺）。
         */
        fun bundled(): AbogusProfile {
            val text = AbogusProfile::class.java
                .getResourceAsStream("/feedhub/source/douyin/abogus_profile.json")
                ?.use { it.readBytes().toString(Charsets.UTF_8) }
                ?: return AbogusProfile()
            return runCatching { parse(text) }.getOrElse { AbogusProfile() }
        }

        /**
         * 随包 JSON 里的那份常量。
         * 名字里的 `LIVE_FROM_YLCANGEL` 是历史：最早这几个值是移植来源（live 子域）的写死值，
         * 含义已经变成"当前基线"了。
         */
        val WWW_MEASURED: AbogusProfile = bundled()
    }
}

/**
 * 签名时用的浏览器环境。这些值参与 a_bogus 的字节，**必须和信息流请求真正声称的一致**
 */
internal data class AbogusEnv(
    val userAgent: String,
    val platform: String = "Win32",
    val innerWidth: Int = 1920,
    val innerHeight: Int = 900,
    val outerWidth: Int = 1920,
    val outerHeight: Int = 1040,
    val availWidth: Int = 1920,
    val availHeight: Int = 1040,
    /**
     * `screen.sizeWidth` / `screen.sizeHeight`：**不是标准属性**，真实 Chrome 里是 `undefined`。
     * 移植来源里那句 `(screen.width >> 0) == 0 ? 2560 : (screen.sizeWidth >> 0)` 在真实浏览器里
     * `undefined >> 0` 取 0，所以默认就是 0。
     */
    val screenSizeWidth: Int = 0,
    val screenSizeHeight: Int = 0,
    /**
     * 直接用这个窗口指纹串（从**真实浏览器**里拿到的原串），给了就不按上面那些字段拼。
     *
     * 之所以需要它：指纹串是 bdms **内部**按 `Object.defineProperty` 的写入顺序拼出来的，
     * 这里那些字段名和顺序是从移植来源反推的，和真实实现可能差一项。
     * 事后观察的抓包（`capture/douyin-abogus-observer.js`）不需要它：工具能从签名里**反解**出
     * bdms 当时真正用的那一串（`AbogusReverse`），比猜拼法可靠。
     */
    val fingerprintOverride: String? = null,
) {
    /**
     * `U[76]`：窗口指纹串，按移植来源里 `Object.defineProperty` 的写入顺序拼，末尾不留 `|`。
     * 例：`1920|900|1920|1040|1920|1040|0|0|Win32`。
     */
    fun fingerprint(): String = fingerprintOverride ?: listOf(
        innerWidth, innerHeight, outerWidth, outerHeight,
        availWidth, availHeight, screenSizeWidth, screenSizeHeight, platform,
    ).joinToString("|")
}

/** 随机源。默认是 `Math.random()`；测试里注入固定值，签名就变得完全确定、可比对。 */
internal fun interface AbogusRandom {
    fun nextDouble(): Double

    companion object {
        /** 生产用：和 JS 的 `Math.random()` 一样。 */
        val SYSTEM = AbogusRandom { Math.random() }

        /** 测试用：永远返回同一个值。 */
        fun constant(value: Double) = AbogusRandom { value }
    }
}

/**
 * 抖音 web 端的 `a_bogus` 签名。
 *
 * ## 来源与许可
 *
 * 骨架移植自 `ylcangel/douyin_sign: a_bogus/utils.js`
 * 与同目录的 `sm3.js`：
 * ```
 * Copyright AngelToms
 * SPDX-License-Identifier: Apache-2.0
 * ```
 * （Apache-2.0，按 License 保留上述版权声明。）
 * 两份文件对应的 bdms 版本是 `1.0.1.19-fix.01`。
 *
 * ## 与移植来源的差异（都写在这里，改之前先看这一节）
 *
 * 1. **`random()` 里少了调用括号的地方按"真的调用了"实现**。
 *    移植来源的 `flag === 1` / `flag === 2` 两条分支写成 `Math.random * 40`、`Math.random * 255`
 *    （少了 `()`，JS 里是 `NaN >> 0 = 0`）。实测证据：`samples_v192.json`（真实 Chrome、固定
 *    `Date.now=1700000000000`、`Math.random=0.5`、query=`aweme_id=1`）抓到的 RC4 前 8 字节是
 *    `[171,85,42,85, 41,81,170,85]`。用"真的调用了"算 `flag===2` 那条正好得到 `[41,81,170,85]`；
 *    照抄缺括号的写法会得到 `[41,81,162,16]`（`r2 = 0` 之后还要 `|= 2|16|32|128` 得 178，
 *    178&170=162、178&85=16）。所以这里按真实行为实现，`AbogusSignerTest` 用这条向量把关。
 *    （`flag === 1` 那条没有独立证据，按同样方式修正——两条写法一模一样。）
 * 2. **常量全部走 [AbogusProfile]**，不写死在算法里，方便按子域重抓后只改数据。
 * 3. **`U[13]`（`window.onwheelx`）只用来决定 `U[12]`**，移植来源里已经证成 `U[12] = 3`，
 *    这里直接取 3，不再构造那个对象。
 * 4. 输出长度**不是固定的**，
 *    所以调用方不要按长度校验。
 *
 * ## 参数
 *
 * [uri] 就是 query 串，**不带 `a_bogus` 自己**；每个 value 要先做过百分号编码
 *
 * **POST 的请求体签名没有实现**：当前只用 GET（`module/feed`、`profile/self`），
 * 需要 body 参与签名的都是写接口（评论、点赞），
 * 以后要加的话，照 cv-cat 的说法是 query 和 body 各自 `+ dhzx` 之后各做两次 SM3。
 */
internal class AbogusSigner(
    private val profile: AbogusProfile = AbogusProfile.WWW_MEASURED,
    private val env: AbogusEnv,
    private val random: AbogusRandom = AbogusRandom.SYSTEM,
    private val clock: () -> Long = System::currentTimeMillis,
    /** 进入页面（SDK 加载）的时间，只参与 `U[28]`；默认在构造时取一次，和移植来源一致。 */
    private val enterPageTs: Long = clock(),
) {
    /**
     * 生成 a_bogus。
     *
     * @param uri query 串（已百分号编码，不含 `a_bogus`）
     * @param now 这次签名的时刻（默认取 [clock]；Rust 版把同一个值当参数传，两边才好逐字节比）
     */
    fun sign(uri: String, now: Long = clock()): String {
        val parts = buildParts(uri, now)
        // ncode = U[86] ++ U[90]，RC4 的 key 是单字节 211
        val crypt = dyRc4(intArrayOf(211), parts.u86 + parts.u90)
        // U[92] = U[89] ++ crypt，再走 s4 表 Base64
        return dyBase64(parts.u89 + crypt, "s4")
    }

    /**
     * 算法中间的几段字节。抽出来是为了**能拿真实抓包逐字节对拍**
     * （`samples_v192.json` 抓到的就是 `u86 ++ u90`），见 [AbogusSignerTest]。
     */
    internal data class Parts(
        /** 8 字节随机（`U[86]`）。 */
        val u86: IntArray,
        /** `tlist ++ 窗口指纹 ++ 时间串 ++ [U87]`（`U[88]`），是 [u90] 的输入。 */
        val u88: IntArray,
        /** 4 字节随机头（`U[89]`）。 */
        val u89: IntArray,
        /** `u88Transform(u88)` 的结果（`U[90]`）。 */
        val u90: IntArray,
    ) {
        override fun equals(other: Any?): Boolean = other is Parts &&
            u86.contentEquals(other.u86) && u88.contentEquals(other.u88) &&
            u89.contentEquals(other.u89) && u90.contentEquals(other.u90)

        override fun hashCode(): Int = listOf(u86, u88, u89, u90).fold(1) { acc, a -> 31 * acc + a.contentHashCode() }
    }

    /** 算到 RC4 之前为止的所有中间字节。 */
    internal fun buildParts(uri: String, now: Long = clock()): Parts {
        val u = HashMap<Int, Int>()
        fun i(index: Int): Int = u[index] ?: error("U[$index] 还没赋值（算法移植有误）")
        fun set(index: Int, value: Int) {
            u[index] = value
        }

        val ink = now - 1

        // U[18] / U[19] / U[21]：三处 SM3
        val urlSm3 = Sm3.digestTwice((if (uri.endsWith(SALT)) uri else uri + SALT).toByteArray(Charsets.UTF_8))
        val haltSm3 = Sm3.digestTwice(SALT.toByteArray(Charsets.UTF_8))
        val ua = env.userAgent.trim()
        val uaEncode = dyBase64(dyRc4(createKey(), ua.toCharArray().map { it.code }.toIntArray()), "s3")
        val uaeSm3 = Sm3.digest(uaEncode.toByteArray(Charsets.ISO_8859_1))

        // U[2..10]
        set(2, 1)
        set(3, 0)
        set(4, 8)
        set(6, 0)
        set(8, profile.pageId)
        set(9, profile.aid)

        // U[12] / U[16] / U[17]
        set(12, 3)
        set(16, 1)
        set(17, profile.u17)

        set(26, ((now - TWO_WEEK_EPOCH).toDouble() / 1000 / 60 / 60 / 24 / 14).toInt())
        set(27, profile.counter)
        set(24, 41)

        // U[25]：SDK 版本按 "." 拆开
        val versionArray = profile.sdkVersion.split(".").map { jsToInt32(it) }.toIntArray()

        // U[22] = ink
        // U[28..36]
        // 注意：JS 里 U[14] / U[22] 是 **double**（`+Date.now()`，1.7e12），
        // `& 255` / `>> n` 会先做 ToInt32，但 `/ 256^4`、`/ 256^5` 是在那个 double 上直接除的。
        // 所以这里保留 Long 原值，只有需要"取某个字节"时才往下截。
        set(28, ((now - enterPageTs + 3) and 255L).toInt())
        set(29, (now and 255L).toInt())
        set(30, ((now shr 8) and 255L).toInt())
        set(31, ((now shr 16) and 255L).toInt())
        set(32, ((now shr 24) and 255L).toInt())
        set(33, ((now / 4294967296L) and 255L).toInt())
        set(34, ((now / 1099511627776L) and 255L).toInt())
        set(35, i(16) % 256 and 255)
        set(36, i(16) / 256 and 255)

        // U[37..47]
        val u37 = profile.u37
        set(38, u37[4] and 255)
        set(39, (u37[4] shr 8) and 255)
        set(40, u37[0])
        set(41, u37[1])
        set(42, u37[2])
        set(43, u37[3])
        set(44, i(17) and 255)
        set(45, (i(17) shr 8) and 255)
        set(46, (i(17) shr 16) and 255)
        set(47, (i(17) shr 24) and 255)

        // U[48..59]：三份 SM3 摘要里挑出来的字节
        set(48, urlSm3[9].toInt() and 255)
        set(49, urlSm3[18].toInt() and 255)
        set(51, urlSm3[3].toInt() and 255)
        set(52, haltSm3[10].toInt() and 255)
        set(53, haltSm3[19].toInt() and 255)
        set(55, haltSm3[4].toInt() and 255)
        set(56, uaeSm3[11].toInt() and 255)
        set(57, uaeSm3[21].toInt() and 255)
        set(59, uaeSm3[5].toInt() and 255)

        // U[60..65] = ink 的各个字节（ink 同样是 double，见上面 U[28..36] 的说明）
        set(60, (ink and 255L).toInt())
        set(61, ((ink shr 8) and 255L).toInt())
        set(62, ((ink shr 16) and 255L).toInt())
        set(63, ((ink shr 24) and 255L).toInt())
        set(64, ((ink / 4294967296L) and 255L).toInt())
        set(65, ((ink / 1099511627776L) and 255L).toInt())
        set(66, i(12))

        // U[67..74] = pageId / aid 的字节
        set(67, i(8) and 255)
        set(68, (i(8) shr 8) and 255)
        set(69, (i(8) shr 16) and 255)
        set(70, (i(8) shr 24) and 255)
        set(71, i(9) and 255)
        set(72, (i(9) shr 8) and 255)
        set(73, (i(9) shr 16) and 255)
        set(74, (i(9) shr 24) and 255)

        // U[76..85]：窗口指纹串 + "(时间+3)," 的字符码
        val fingerprint = env.fingerprint()
        set(78, fingerprint.length)
        set(79, fingerprint.length and 255)
        set(80, (fingerprint.length shr 8) and 255)
        val u81 = "${((now + 3) and 255L).toInt()},"
        set(83, u81.length)
        set(84, u81.length and 255)
        set(85, (u81.length shr 8) and 255)

        // U[86]：8 字节随机（两个 random 调用）
        val vor = intArrayOf(versionArray[0], versionArray[1])
        val u86 = random(vor, null, 1) + random(vor, 2, 2)

        // U[87]：一串字段异或
        var u87 = 0
        for (b in u86) u87 = u87 xor b
        for (index in U87_INDICES) u87 = u87 xor i(index)

        // tlist（顺序和移植来源逐项一致，**不要重排**）
        val tlist = intArrayOf(
            i(34), i(44), i(56), i(61), i(73), i(29), i(70), i(45), i(35), i(49),
            i(38), i(66), i(51), i(68), i(28), i(48), i(64), i(47), i(30), i(71),
            i(26), i(55), i(31), i(69), i(59), i(40), i(62), i(63), i(27), i(72),
            i(41), i(74), i(57), i(52), i(42), i(39), i(33), i(67), i(53), i(43),
            i(65), i(46), i(36), i(24), i(60), i(32), i(79), i(80), i(84), i(85),
        )

        val u88 = tlist + fingerprint.toCharArray().map { it.code } +
            u81.toCharArray().map { it.code } + intArrayOf(u87)

        // U[89]：4 字节随机头
        val u89 = random(intArrayOf(3, 82), 1, 2)

        // U[90]
        val u90 = u88Transform(u88)

        return Parts(u86 = u86, u88 = u88, u89 = u89, u90 = u90)
    }

    // ---------- 加密原语（都按 JS 的位运算语义实现） ----------

    /**
     * 最终那一步 RC4 的**密钥流**（长度 [length]）。
     *
     * 给对拍工具用：RC4 是流密码，密钥流只取决于密钥（这里固定是单字节 211），
     * 所以拿全 0 明文走一遍 `dyRc4` 得到的就是密钥流；工具用它把抓到的 `a_bogus`
     * 反解回 RC4 之前的明文（`U[86] ++ U[90]`），就能**逐字节**看出差在哪个 U 值上，
     * 比只比 base64 字符串有用得多。见 `dev/PlaintextDiff.kt`。
     */
    internal fun keystream(length: Int): IntArray = dyRc4(intArrayOf(211), IntArray(length))

    /**
     * 魔改 RC4：S 盒初始化为 `255 - i`，密钥调度是 `j = (j * S[i] + j + K[i]) % 256`，PRGA 是标准的。
     * 明文按 JS 的 `charCodeAt` 取（`and 0xFFFF`），密文按 `Uint8Array` 截断（`and 0xFF`）——
     * 这两处截断在移植来源里分别由 `charCodeAt` 和 `cipher = new Uint8Array(...)` 完成。
     */
    private fun dyRc4(key: IntArray, text: IntArray): IntArray {
        val s = IntArray(256) { 255 - it }
        val k = IntArray(256) { key[it % key.size] and 0xFF }
        var j = 0
        for (idx in 0 until 256) {
            j = (j * s[idx] + j + k[idx]) % 256
            val tmp = s[idx]
            s[idx] = s[j]
            s[j] = tmp
        }
        val out = IntArray(text.size)
        var a = 0
        var b = 0
        for (n in text.indices) {
            a = (a + 1) % 256
            b = (b + s[a]) % 256
            val tmp = s[a]
            s[a] = s[b]
            s[b] = tmp
            val rnd = s[(s[a] + s[b]) % 256]
            out[n] = ((text[n] and 0xFFFF) xor rnd) and 0xFF
        }
        return out
    }

    /**
     * 5 张自定义 Base64 表里的 [table]（`s0`~`s4`）。逐 3 字节取，不足补 `=`，
     * 和移植来源的 `dyBase64` 一致（`s3` 用于 UA，`s4` 用于最终结果）。
     */
    private fun dyBase64(bytes: IntArray, table: String): String {
        val t = TABLES[table] ?: error("未知的 Base64 表：$table")
        val sb = StringBuilder()
        var i = 0
        while (i < bytes.size) {
            val c1 = bytes[i] and 255
            val c2: Int? = bytes.getOrNull(i + 1)?.and(255)
            val c3: Int? = bytes.getOrNull(i + 2)?.and(255)
            val c = (c1 shl 16) or ((c2 ?: 0) shl 8) or (c3 ?: 0)
            sb.append(t[(c and 16515072) shr 18])
            sb.append(t[(c and 258048) shr 12])
            if (c2 != null) sb.append(t[(c and 4032) shr 6])
            if (c3 != null) sb.append(t[c and 63])
            if (c2 == null) sb.append('=')
            if (c3 == null) sb.append('=')
            i += 3
        }
        return sb.toString()
    }

    /** `createKey()`：`String.fromCharCode(1/256, 1%256, sdkMinor)` → 前两个是 0 和 1。 */
    private fun createKey(): IntArray = intArrayOf(0, 1, profile.sdkMinor and 0xFF)

    /**
     * `u88Transform`：每 3 字节混进一个随机数变 4 字节；不满 3 字节的尾巴照原样带上。
     * `E1..E6` 是移植来源里写死的常量。
     */
    internal fun u88Transform(array: IntArray): IntArray {
        val out = ArrayList<Int>(array.size * 4 / 3 + 2)
        var i = 0
        while (i < array.size) {
            if (i + 2 < array.size) {
                val x = toInt32(random.nextDouble() * 1000) and 255
                val a = (x and E1) or (array[i] and E2)
                val b = (x and E3) or (array[i + 1] and E4)
                val c = (x and E5) or (array[i + 2] and E6)
                val d = (array[i] and E1) or (array[i + 1] and E3) or (array[i + 2] and E5)
                out += a
                out += b
                out += c
                out += d
            } else {
                out += array[i]
                array.getOrNull(i + 1)?.let { out += it }
            }
            i += 3
        }
        return out.toIntArray()
    }

    /**
     * `random(inObj)`：把一个 2 字节输入和随机数按位混成 4 字节。
     *
     * `flag` 决定 r1 / r2 怎么来：`length > 1` 且 `inObj["1"]` 存在时 `flag = inObj["1"]`。
     * `flag == 1` / `flag == 2` 两条分支在移植来源里少了 `Math.random` 的调用括号，
     * 这里按真实行为实现（理由见 [AbogusSigner] 类说明第 1 条）。
     */
    internal fun random(zero: IntArray, oneValue: Int?, length: Int): IntArray {
        val in0 = zero[0]
        val in1 = zero[1]
        val r = random.nextDouble() * 65535
        val ri = toInt32(r)
        var r1 = ri and 255
        var r2 = (ri shr 8) and 255

        val flag = if (length > 1) (oneValue ?: 0) else 0
        if (flag == 1) {
            r2 = toInt32(random.nextDouble() * 40)
        }
        if (flag == 2) {
            r1 = toInt32(random.nextDouble() * 240)
            if (r1 > 109) {
                r1 += r1 % 2
                r1 += 1
            }
            r2 = toInt32(random.nextDouble() * 255) and 77
            r2 = r2 or (1 shl 1) or (1 shl 4) or (1 shl 5) or (1 shl 7)
        }

        val u1 = (r1 and 170) or (in0 and 85)
        val u2 = (r1 and 85) or (in0 and 170)
        val u3 = (r2 and 170) or (in1 and 85)
        val u4 = (r2 and 85) or (in1 and 170)
        return intArrayOf(u1, u2, u3, u4)
    }

    /**
     * JS 的 `x | 0`（ToInt32）：向零截断。这里的入参都远小于 2^31，不会溢出，
     * 所以 Kotlin 的 `Double.toInt()`（同样是向零截断）语义一致。
     */
    private fun toInt32(value: Double): Int = value.toInt()

    /** JS 的 `~x; ~x`（= ToInt32），用来把 SDK 版本号各段变成整数；整段不是数字时 JS 得 NaN → 0。 */
    private fun jsToInt32(text: String): Int {
        val t = text.trim()
        if (!JS_NUMBER.matches(t)) return 0
        return t.toDouble().toInt()
    }

    private companion object {
        const val SALT = "dhzx"
        const val TWO_WEEK_EPOCH = 1721836800000L
        const val E1 = 145
        const val E2 = 110
        const val E3 = 66
        const val E4 = 189
        const val E5 = 44
        const val E6 = 211

        /** `U[87]` 里除 `U[86]` 的 8 个字节之外要异或的 U 下标（顺序无关，照移植来源抄的清单）。 */
        val U87_INDICES = intArrayOf(
            24, 26, 27, 28, 29, 30, 31, 32, 33, 34, 35, 36,
            38, 39, 40, 41, 42, 43, 44, 45, 46, 47, 48, 49,
            51, 52, 53, 55, 56, 57, 59, 60, 61, 62, 63, 64,
            65, 66, 67, 68, 69, 70, 71, 72, 73, 74,
            79, 80, 84, 85,
        )

        /** JS 里 `Number("...")` 认的写法（整段都要是数字，否则 NaN）。 */
        val JS_NUMBER = Regex("""^[+-]?(\d+\.?\d*|\.\d+)([eE][+-]?\d+)?$""")

        val TABLES = mapOf(
            "s0" to "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/=",
            "s1" to "Dkdpgh4ZKsQB80/Mfvw36XI1R25+WUAlEi7NLboqYTOPuzmFjJnryx9HVGcaStCe=",
            "s2" to "Dkdpgh4ZKsQB80/Mfvw36XI1R25-WUAlEi7NLboqYTOPuzmFjJnryx9HVGcaStCe=",
            "s3" to "ckdp1h4ZKsUB80/Mfvw36XIgR25+WQAlEi7NLboqYTOPuzmFjJnryx9HVGDaStCe",
            "s4" to "Dkdpgh2ZmsQB80/MfvV36XI1R45-WUAlEixNLwoqYTOPuzKFjJnry79HbGcaStCe",
        )
    }
}
