package feedhub.app.ui

import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.text.buildAnnotatedString
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.withStyle

/**
 * 彩色 emoji 字体族。
 *
 * **为什么需要它**：桌面上 Compose 的字体回退是问 Skia 的 `FontCollection`，而 fontconfig 对
 * "要 `U+FE0F`（变体选择符）才彩色"的那一批 emoji（`⚡️` `❤️` `☺️` `⚠️` …）排在前面的是
 * `Noto Sans Symbols` / `DejaVu Sans`（**黑白字形**），`Noto Color Emoji` 排在后面 —— 于是它们画出来是
 * 黑白轮廓，而 `U+1F300` 以上的（`😭` `👍` `🇨🇳`）只命中彩色字体，看着就是"有的表情渲染了、有的没渲染"。
 *
 * 安卓不需要这个兜底：系统回退链里 `NotoColorEmoji` 就是彩色那一个，`U+263A+FE0F` 本来就彩色，
 * 所以安卓的 actual 返回 null，那边一个像素都不变。
 *
 * **只给含 emoji 的那几段文字套**（[emojiRuns]）：整段套上去会把中文也送进 emoji 字体（那里没有汉字，
 * 还是得回退，白白多一次字体查询）。
 */
expect fun colorEmojiFontFamily(): FontFamily?

/**
 * 文本里所有 emoji 片段（UTF-16 下标，左闭右闭）。
 *
 * 认的是 Unicode 的 **Emoji_Presentation（默认彩色）** 那批，外加"文字默认、后面跟了 `U+FE0F` 要彩色"的：
 * - 基础字符：`U+1F000..U+1FAFF`（各 emoji 块 + 国旗的区域指示符）、以及 BMP 里默认彩色的那批
 *   （`⌚ ⚡ ✅ ✨ ❗ ⭐ ⭕` …，见 [EMOJI_PRESENTATION_BMP]）；
 * - 扩展：`U+FE0F` / `U+FE0E`、肤色修饰 `U+1F3FB..U+1F3FF`、`U+20E3`（keycap）、标签字符
 *   `U+E0020..U+E007F`、以及 **ZWJ** 后面接的另一个 emoji（`👨‍👩‍👧‍👦`、`🏳️‍🌈`）；
 * - 特殊：`#` `*` `0`–`9` 后面跟 `U+FE0F`（或直接跟 `20E3`）算一个 keycap emoji（`1️⃣`）。
 *
 * 只用来给这几段套字体族，**不改文字**（复制 / 分享 / 搜索拿到的还是原文）。
 */
fun emojiRuns(text: String): List<IntRange> {
    if (text.isEmpty()) return emptyList()
    val out = ArrayList<IntRange>()
    var i = 0
    while (i < text.length) {
        val cp = codePointAt(text, i)
        val len = charCount(cp)
        val startsEmoji = isEmojiPresentation(cp) ||
            (isEmojiTextBase(cp) && followedByVs16(text, i + len)) ||
            (isKeycapBase(cp) && (followedByVs16(text, i + len) || followedByCodePoint(text, i + len, KEYCAP)))
        if (!startsEmoji) {
            i += len
            continue
        }
        var end = i + len
        if (isKeycapBase(cp) && followedByVs16(text, end)) end += 1
        while (end < text.length) {
            val n = codePointAt(text, end)
            val nlen = charCount(n)
            when {
                n == VS16 || n == VS15 -> end += nlen
                n in SKIN_TONES -> end += nlen
                n == KEYCAP -> end += nlen
                n in TAG_RANGE -> end += nlen
                n == ZWJ -> {
                    // ZWJ 只有在**后面还接一个 emoji** 时才算同一个片段；否则它就是普通字符，停在这里
                    val after = end + nlen
                    if (after >= text.length) break
                    val a = codePointAt(text, after)
                    val alen = charCount(a)
                    when {
                        isEmojiPresentation(a) -> end = after + alen
                        isEmojiTextBase(a) && followedByVs16(text, after + alen) -> end = after + alen + 1
                        else -> break
                    }
                }
                // 国旗：两个区域指示符连一起是一个字
                isRegionalIndicator(cp) && isRegionalIndicator(n) && end == i + len -> end += nlen
                else -> break
            }
        }
        out += i until end
        i = end
    }
    return out
}

/**
 * [text] → 带彩色 emoji 字体的 [AnnotatedString]；这一端不需要兜底 / 文字里没有 emoji 时返回 `null`
 * （调用方继续用纯 [String] 那条快路径，一个字都不变）。
 */
fun colorEmojiText(text: String, family: FontFamily? = colorEmojiFontFamily()): AnnotatedString? {
    if (family == null || text.isEmpty()) return null
    val runs = emojiRuns(text)
    if (runs.isEmpty()) return null
    val style = SpanStyle(fontFamily = family)
    return buildAnnotatedString {
        var last = 0
        for (r in runs) {
            if (r.first > last) append(text.substring(last, r.first))
            withStyle(style) { append(text.substring(r.first, r.last + 1)) }
            last = r.last + 1
        }
        if (last < text.length) append(text.substring(last))
    }
}

/**
 * 给已经有样式的 [text] 补上彩色 emoji 字体（别的样式不动）：`weight` 传了就给 emoji 那几段也用上
 * （标题是 SemiBold，emoji 位图字体没有粗体，套上去会被合成加粗；传 null 就沿用原来的字重）。
 */
fun AnnotatedString.withColorEmoji(
    family: FontFamily? = colorEmojiFontFamily(),
    weight: FontWeight? = null,
): AnnotatedString {
    if (family == null) return this
    val runs = emojiRuns(text)
    if (runs.isEmpty()) return this
    val style = SpanStyle(fontFamily = family, fontWeight = weight)
    return AnnotatedString.Builder(this).apply {
        for (r in runs) addStyle(style, r.first, r.last + 1)
    }.toAnnotatedString()
}

private const val VS15 = 0xFE0E
private const val VS16 = 0xFE0F
private const val ZWJ = 0x200D
private const val KEYCAP = 0x20E3
private val SKIN_TONES = 0x1F3FB..0x1F3FF
private val TAG_RANGE = 0xE0020..0xE007F

/** 补充平面 emoji 的主块（含区域指示符 `U+1F1E6..U+1F1FF` 和肤色修饰）。 */
private val SUPPLEMENTARY_EMOJI = 0x1F000..0x1FAFF

/**
 * Unicode `Emoji_Presentation=Yes` 的 BMP 部分（这些**不带** `U+FE0F` 也应当是彩色）。星象符号也在里面，
 * 所以用区间。表外的 BMP 符号（`☺ ❤ ✈ ☀ ⚠` 等）是"文字默认"，只有后面跟了 `U+FE0F` 才算 emoji（[isEmojiTextBase]）。
 */
private val EMOJI_PRESENTATION_BMP: List<IntRange> = listOf(
    0x231A..0x231B, 0x23E9..0x23EC, 0x23F0..0x23F0, 0x23F3..0x23F3,
    0x25FD..0x25FE, 0x2614..0x2615, 0x2648..0x2653, 0x267F..0x267F,
    0x2693..0x2693, 0x26A1..0x26A1, 0x26AA..0x26AB, 0x26BD..0x26BE,
    0x26C4..0x26C5, 0x26CE..0x26CE, 0x26D4..0x26D4, 0x26EA..0x26EA,
    0x26F2..0x26F3, 0x26F5..0x26F5, 0x26FA..0x26FA, 0x26FD..0x26FD,
    0x2705..0x2705, 0x270A..0x270B, 0x2728..0x2728, 0x274C..0x274C,
    0x274E..0x274E, 0x2753..0x2755, 0x2757..0x2757, 0x2795..0x2797,
    0x27B0..0x27B0, 0x27BF..0x27BF, 0x2B1B..0x2B1C, 0x2B50..0x2B50,
    0x2B55..0x2B55,
)

/** 默认就是彩色的 emoji（补充平面全部 + BMP 那批）。 */
private fun isEmojiPresentation(cp: Int): Boolean =
    cp in SUPPLEMENTARY_EMOJI || EMOJI_PRESENTATION_BMP.any { cp in it }

/**
 * "文字默认"、可以靠 `U+FE0F` 变成彩色的 BMP 符号（`☺ ☹ ☠ ☢ ☣ ☦ ☪ ☸ ♀ ♂ ⚕ ⚖ ⚗ ⚙ ⚰ ⚱ ⚔ ⚜ ⚠ ⛎ ⛔…`、
 * `✂ ✈ ✉ ✌ ✍ ✏ ✒ ✔ ✖ ✝ ✡ ✳ ✴ ❄ ❇ ❌ ❎ ❓ ❔ ❕ ❗ ❣ ❤ ➡ ⤴ ⤵ ↩ ↪` 等等）。
 * 这里**只用来决定要不要看后面的 `U+FE0F`**，范围放宽松没坏处：真正的判定是"后面有没有 FE0F"。
 */
private fun isEmojiTextBase(cp: Int): Boolean = when (cp) {
    0x00A9, 0x00AE, 0x2122, 0x2139, 0x3030, 0x303D, 0x3297, 0x3299 -> true
    in 0x203C..0x2049, in 0x2194..0x21AA, in 0x24C2..0x24C2 -> true
    in 0x25AA..0x25FE, in 0x2600..0x27BF, in 0x2934..0x2935, in 0x2B05..0x2B55 -> true
    else -> false
}

/** keycap 的底：`#` `*` 和数字。它们单独出现是普通字符，只有跟 `FE0F` / `20E3` 才是 emoji。 */
private fun isKeycapBase(cp: Int): Boolean = cp == '#'.code || cp == '*'.code || cp in '0'.code..'9'.code

/** 区域指示符（国旗字母）：`U+1F1E6..U+1F1FF`，两个连一起是一个国家。 */
private fun isRegionalIndicator(cp: Int): Boolean = cp in 0x1F1E6..0x1F1FF

/** `text[i]` 起的一个码点（`commonMain` 里没有 `Character.codePointAt`，自己按代理对拆）。 */
private fun codePointAt(text: String, index: Int): Int {
    val c = text[index]
    if (c.isHighSurrogate() && index + 1 < text.length) {
        val low = text[index + 1]
        if (low.isLowSurrogate()) return 0x10000 + ((c.code - 0xD800) shl 10) + (low.code - 0xDC00)
    }
    return c.code
}

private fun charCount(cp: Int): Int = if (cp >= 0x10000) 2 else 1

private fun followedByVs16(text: String, index: Int): Boolean = followedByCodePoint(text, index, VS16)

private fun followedByCodePoint(text: String, index: Int, cp: Int): Boolean =
    index < text.length && codePointAt(text, index) == cp
