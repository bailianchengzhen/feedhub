package feedhub.app

import android.content.Context
import kotlinx.coroutines.CoroutineScope

/**
 * 基础版的空实现（未开启 `-Pfeedhub.embed=true` / `-Pfeedhub.embedNpu=true`）：
 * 不引 `onnxruntime-android` / QNN，不建任何本地向量或封面向量会话。
 */
internal fun createAndroidEmbedBundle(
    appContext: Context,
    scope: CoroutineScope,
    log: (String) -> Unit,
    includeCover: Boolean = true,
): AndroidEmbedBundle = AndroidEmbedBundle()
