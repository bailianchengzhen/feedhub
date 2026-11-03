package feedhub.cli

import com.google.zxing.BarcodeFormat
import com.google.zxing.EncodeHintType
import com.google.zxing.common.BitMatrix
import com.google.zxing.qrcode.QRCodeWriter
import com.google.zxing.qrcode.decoder.ErrorCorrectionLevel
import java.awt.image.BufferedImage
import java.nio.file.Files
import java.nio.file.Path
import javax.imageio.ImageIO

/**
 * 把二维码内容（[feedhub.core.QrLogin.content]）画出来：终端里用 Unicode 半块字符，另存一张 PNG。
 * 编码用 zxing（`com.google.zxing:core`），前端以后画二维码也用它。
 */
object QrRender {
    /** 二维码矩阵，不含静区（静区在画的时候加）。纠错等级 M：终端字体行距会让半块字符之间出细缝，留点余量。 */
    fun encode(content: String): BitMatrix {
        val hints = mapOf(
            EncodeHintType.MARGIN to 0,
            EncodeHintType.ERROR_CORRECTION to ErrorCorrectionLevel.M,
            EncodeHintType.CHARACTER_SET to "UTF-8",
        )
        return QRCodeWriter().encode(content, BarcodeFormat.QR_CODE, 0, 0, hints)
    }

    /**
     * 终端里的二维码：两行模块合成一行字符（`▀ ▄ █` 和空格），每个模块在终端里接近正方形。
     * [ansi] 为真时每行套上 ANSI 的"黑字白底"（`ESC[30;47m`），这样深色 / 浅色主题的终端里都是白底黑码，手机能直接扫；
     * 为假时只输出字符（测试里用，还原成矩阵再解码）。静区 [quiet] 个模块，四周都留。
     */
    fun toTerminal(matrix: BitMatrix, quiet: Int = 3, ansi: Boolean = true): String {
        val size = matrix.width + 2 * quiet
        fun dark(x: Int, y: Int): Boolean {
            val mx = x - quiet
            val my = y - quiet
            return mx in 0 until matrix.width && my in 0 until matrix.height && matrix.get(mx, my)
        }
        val sb = StringBuilder()
        for (y in 0 until size step 2) {
            if (ansi) sb.append("\u001b[30;47m")
            for (x in 0 until size) {
                sb.append(
                    when {
                        dark(x, y) && dark(x, y + 1) -> '█'
                        dark(x, y) -> '▀'
                        dark(x, y + 1) -> '▄'
                        else -> ' '
                    },
                )
            }
            if (ansi) sb.append("\u001b[0m")
            sb.append('\n')
        }
        return sb.toString()
    }

    /** PNG：白底黑码，每个模块 [scale] 像素，静区 [quiet] 个模块（二维码规范要求 4）。 */
    fun toPng(matrix: BitMatrix, file: Path, scale: Int = 8, quiet: Int = 4) {
        val size = (matrix.width + 2 * quiet) * scale
        val image = BufferedImage(size, size, BufferedImage.TYPE_BYTE_BINARY)
        val white = 0xFFFFFF
        val black = 0x000000
        for (y in 0 until size) for (x in 0 until size) {
            val mx = x / scale - quiet
            val my = y / scale - quiet
            val isDark = mx in 0 until matrix.width && my in 0 until matrix.height && matrix.get(mx, my)
            image.setRGB(x, y, if (isDark) black else white)
        }
        file.toAbsolutePath().parent?.let { Files.createDirectories(it) }
        check(ImageIO.write(image, "png", file.toFile())) { "写不了 PNG：${file}" }
    }
}
