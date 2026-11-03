package feedhub.app.translate

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.Translate
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.Immutable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.input.pointer.PointerIcon
import androidx.compose.ui.input.pointer.pointerHoverIcon
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp

/**
 * **「X 式」就地翻译的那一行**。
 *
 * 一条内容（帖子正文 / 视频简介 / 每条评论 / 楼中楼）下面的一行小字，一共五种样子：
 *
 * ```
 *                  翻译                     ← 强调色小字，**只在判定为外语时出现**（中文内容不出现）
 *              正在翻译…                    ← 点了之后（模型还在加载时是「正在加载翻译模型…」）
 *    由本地模型翻译自 英语 · 显示原文          ← 翻好之后（正文已经就地换成译文），点它切回原文
 *    翻译失败：模型没有返回译文 · 重试          ← 失败：一行灰字原因 +「重试」
 * ```
 *
 * 这一层只画。**"该不该出现 / 现在哪一档 / 显示原文还是译文"全在 Rust**
 * （`feedhub-core/src/translate/entry.rs`，经 [TranslationStore.phase] / [TranslationStore.entryVisible]）；
 * 文案和字号在这里定。
 *
 * 评论那一级小一号（[compact]）：和 X 一样，评论里的这行比正文那行小、留白也小。
 */
@Composable
fun TranslationEntryRow(entry: TranslationEntryState?, compact: Boolean = false) {
    if (entry == null || entry.phase == TranslationPhase.HIDDEN) return
    val emphasis = MaterialTheme.colorScheme.primary
    val muted = MaterialTheme.colorScheme.onSurfaceVariant
    val small = if (compact) 11.sp else 12.sp
    val body = if (compact) 12.sp else 13.sp
    Column(Modifier.padding(top = if (compact) 2.dp else 4.dp)) {
        when (entry.phase) {
            // 可点的那一行：强调色 + 一个小图标。手机上是「点了才翻」
            TranslationPhase.IDLE -> Row(
                Modifier.clip(RoundedCornerShape(50))
                    .pointerHoverIcon(PointerIcon.Hand)
                    .clickable(onClick = entry.onStart)
                    .padding(vertical = 4.dp, horizontal = 2.dp),
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(4.dp),
            ) {
                Icon(
                    Icons.Outlined.Translate,
                    contentDescription = null,
                    tint = emphasis,
                    modifier = Modifier.size(if (compact) 13.dp else 14.dp),
                )
                Text("翻译", fontSize = body, fontWeight = FontWeight.Medium, color = emphasis)
            }
            TranslationPhase.MODEL_LOADING -> Text(
                "正在加载翻译模型…",
                fontSize = body,
                color = muted,
                modifier = Modifier.padding(vertical = 4.dp),
            )
            TranslationPhase.TRANSLATING -> Text(
                "正在翻译…",
                fontSize = body,
                color = muted,
                modifier = Modifier.padding(vertical = 4.dp),
            )
            // 翻好了：一行灰字 + 一个能点的「显示原文 / 显示译文」（**不重新翻**，走缓存）
            TranslationPhase.DONE -> Row(
                Modifier.padding(vertical = 2.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Text("由本地模型翻译自 ${entry.language} · ", fontSize = small, color = muted)
                Text(
                    if (entry.showOriginal) "显示译文" else "显示原文",
                    fontSize = small,
                    color = emphasis,
                    fontWeight = FontWeight.Medium,
                    modifier = Modifier.pointerHoverIcon(PointerIcon.Hand)
                        .clickable(onClick = entry.onToggle)
                        .padding(vertical = 2.dp),
                )
            }
            // 失败：一行灰字原因（引擎 / 模型说的话原样带过来）+「重试」
            TranslationPhase.FAILED -> Row(
                Modifier.padding(vertical = 2.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Text(
                    "翻译失败：${entry.failure ?: "翻译失败"} · ",
                    fontSize = small,
                    color = muted,
                )
                Text(
                    "重试",
                    fontSize = small,
                    color = emphasis,
                    fontWeight = FontWeight.Medium,
                    modifier = Modifier.pointerHoverIcon(PointerIcon.Hand)
                        .clickable(onClick = entry.onRetry)
                        .padding(vertical = 2.dp),
                )
            }
            TranslationPhase.HIDDEN -> Unit
        }
    }
}

/**
 * 一行入口要画的全部东西（由 [rememberTranslationEntry] 从 [TranslationStore] 算出来）。
 *
 * 标 [Immutable]：里面全是值 + 三个 lambda，外层重组时不该因为换了一份新对象而重画整行。
 */
@Immutable
class TranslationEntryState(
    val phase: TranslationPhase,
    /** 灰字里那个语言名（`英语` / `日语` / `外语`）。 */
    val language: String,
    val showOriginal: Boolean,
    /** 失败原因（[TranslationPhase.FAILED] 时那一行灰字）。 */
    val failure: String?,
    /** 点「翻译」——**开始翻**（非挂起：在 store 自己的 scope 里跑）。 */
    val onStart: () -> Unit,
    /** 点「显示原文 / 显示译文」——只切显示，不重新翻。 */
    val onToggle: () -> Unit,
    /** 点「重试」。 */
    val onRetry: () -> Unit,
)
