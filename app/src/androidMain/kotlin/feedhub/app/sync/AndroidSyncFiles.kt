package feedhub.app.sync

import android.content.Context
import android.net.Uri
import android.provider.OpenableColumns
import androidx.activity.ComponentActivity
import androidx.activity.result.ActivityResultLauncher
import androidx.activity.result.contract.ActivityResultContracts
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import feedhub.core.sync.LibrarySyncFile

/**
 * 安卓版的选文件层：系统文档选择器（`ACTION_CREATE_DOCUMENT` / `ACTION_OPEN_DOCUMENT` 的
 * ActivityResult 契约），**不用** `MANAGE_EXTERNAL_STORAGE`，也不在后台碰文件。
 *
 * 只做"选位置 / 读写字节"；加密和合并在 [LibrarySync] 里。
 *
 * `AppGraph` 提升到进程级后（A4），这里按 Activity 生命周期在 `onCreate` / `onDestroy` 挂接与解绑，
 * 避免长生命周期单例一直抓着已销毁的 Activity。
 */
class AndroidSyncFiles(initialActivity: ComponentActivity? = null) : SyncFiles {
    @Volatile
    private var appContext: Context? = initialActivity?.applicationContext

    @Volatile
    private var activity: ComponentActivity? = null

    @Volatile
    private var createDocument: ActivityResultLauncher<String>? = null

    @Volatile
    private var openDocument: ActivityResultLauncher<Array<String>>? = null

    private var pendingSave: CompletableDeferred<Uri?>? = null
    private var pendingOpen: CompletableDeferred<Uri?>? = null

    init {
        if (initialActivity != null) {
            attach(initialActivity)
        }
    }

    /** 必须在 `ComponentActivity.onCreate`（`STARTED` 之前）调用。 */
    fun attach(activity: ComponentActivity) {
        appContext = activity.applicationContext
        this.activity = activity
        createDocument = activity.activityResultRegistry.register(
            KEY_CREATE_DOCUMENT,
            activity,
            ActivityResultContracts.CreateDocument("application/octet-stream"),
        ) { uri ->
            pendingSave?.complete(uri)
            pendingSave = null
        }
        openDocument = activity.activityResultRegistry.register(
            KEY_OPEN_DOCUMENT,
            activity,
            ActivityResultContracts.OpenDocument(),
        ) { uri ->
            pendingOpen?.complete(uri)
            pendingOpen = null
        }
    }

    fun detach(activity: ComponentActivity) {
        if (this.activity === activity) {
            this.activity = null
            createDocument = null
            openDocument = null
            if (!activity.isChangingConfigurations) {
                pendingSave?.complete(null)
                pendingSave = null
                pendingOpen?.complete(null)
                pendingOpen = null
            }
        }
    }

    private fun requireResolver() =
        (activity ?: appContext)?.contentResolver ?: throw IllegalStateException("当前没有可用的应用上下文")

    override suspend fun save(defaultName: String, bytes: ByteArray): String? {
        val uri = withContext(Dispatchers.Main) {
            val launcher = createDocument ?: throw IllegalStateException("当前没有可用的界面窗口")
            val deferred = CompletableDeferred<Uri?>()
            pendingSave = deferred
            launcher.launch(defaultName)
            deferred.await()
        } ?: return null
        val resolver = requireResolver()
        withContext(Dispatchers.IO) {
            val out = resolver.openOutputStream(uri)
                ?: throw IllegalStateException("打不开这个位置")
            out.use { it.write(bytes) }
        }
        return displayName(uri) ?: defaultName
    }

    override suspend fun open(): SyncPickedFile? {
        val uri = withContext(Dispatchers.Main) {
            val launcher = openDocument ?: throw IllegalStateException("当前没有可用的界面窗口")
            val deferred = CompletableDeferred<Uri?>()
            pendingOpen = deferred
            launcher.launch(arrayOf("application/octet-stream", "*/*"))
            deferred.await()
        } ?: return null
        val resolver = requireResolver()
        val bytes = withContext(Dispatchers.IO) {
            val size = resolver.openAssetFileDescriptor(uri, "r")?.use { it.length } ?: -1L
            if (size > LibrarySyncFile.MAX_FILE_BYTES) {
                throw IllegalArgumentException("文件太大（${size / 1024 / 1024} MB）")
            }
            resolver.openInputStream(uri)?.use { input ->
                val buffer = java.io.ByteArrayOutputStream()
                val chunk = ByteArray(1 shl 16)
                while (true) {
                    val n = input.read(chunk)
                    if (n < 0) break
                    if (buffer.size().toLong() + n > LibrarySyncFile.MAX_FILE_BYTES) {
                        throw IllegalArgumentException("文件太大（超过 ${LibrarySyncFile.MAX_FILE_BYTES / 1024 / 1024} MB）")
                    }
                    buffer.write(chunk, 0, n)
                }
                buffer.toByteArray()
            } ?: throw IllegalStateException("读不了这个文件")
        }
        return SyncPickedFile(displayName(uri) ?: "同步文件", bytes)
    }

    private fun displayName(uri: Uri): String? = runCatching {
        requireResolver().query(uri, arrayOf(OpenableColumns.DISPLAY_NAME), null, null, null)?.use { cursor ->
            if (cursor.moveToFirst()) cursor.getString(0) else null
        }
    }.getOrNull()

    private companion object {
        const val KEY_CREATE_DOCUMENT = "feedhub_sync_create_document"
        const val KEY_OPEN_DOCUMENT = "feedhub_sync_open_document"
    }
}
