package feedhub.app.ui

import androidx.compose.ui.text.font.FontFamily

/**
 * 安卓：**不需要**彩色 emoji 的兜底字体族，返回 null。
 *
 * 安卓的字体回退链由系统给，`NotoColorEmoji` 就是彩色那一个（`⚡️ ❤️ ☺️ ⚠️` 这些在系统里本来就是彩色的），
 * 不存在桌面上"先命中 DejaVu / Noto Sans Symbols 的黑白字形"那个问题。返回 null 之后
 * [colorEmojiText] / [AnnotatedString.withColorEmoji] 都是直通，安卓这边一个像素都不变。
 */
actual fun colorEmojiFontFamily(): FontFamily? = null
