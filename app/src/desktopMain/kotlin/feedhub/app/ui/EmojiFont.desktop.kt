package feedhub.app.ui

import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.platform.Font
import java.io.File

/**
 * 桌面：找系统里的**彩色 emoji 字体**（CBDT / COLR 那种位图 / 矢量彩色字体，Linux 上就是 Noto Color Emoji），
 * 用**文件**构造一个 [FontFamily]（见 [EmojiFont] 说的原因）。
 *
 * 为什么是文件而不是家族名：`androidx.compose.ui.text.platform.Font("Noto Color Emoji")` 那个重载把字符串当
 * **类路径资源**（`typefaceResource`），名字不对就静默失败（探针里验证过：不报错、也不生效）。
 *
 * 找不到就返回 null（界面退回原来的行为：emoji 由系统 fallback 决定，黑白就黑白）。
 * 只在第一次问的时候找一遍（[lazy]），之后直接拿缓存。
 */
actual fun colorEmojiFontFamily(): FontFamily? = DesktopEmojiFont.family

internal object DesktopEmojiFont {
    /** 常见发行版里彩色 emoji 的绝对路径（按优先级）。 */
    private val CANDIDATES = listOf(
        "/usr/share/fonts/truetype/noto/NotoColorEmoji.ttf",
        "/usr/share/fonts/noto/NotoColorEmoji.ttf",
        "/usr/share/fonts/noto-emoji/NotoColorEmoji.ttf",
        "/usr/share/fonts/google-noto-emoji/NotoColorEmoji.ttf",
        "/usr/share/fonts/truetype/emoji/NotoColorEmoji.ttf",
    )

    /** `FONTCONFIG_PATH` 之类的路径不参与：这里只要"有这一个文件"，不做 fontconfig 查询（那要桌面 toolkit）。 */
    val family: FontFamily? by lazy { find() }

    private fun find(): FontFamily? {
        val file = CANDIDATES.map(::File).firstOrNull { it.isFile && it.length() > 0 } ?: return null
        return runCatching {
            FontFamily(Font(file, FontWeight.Normal, FontStyle.Normal))
        }.getOrNull()
    }
}
