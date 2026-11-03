package feedhub.app.translate

import feedhub.translate.RustTranslateQueue
import feedhub.translate.TranslateJob
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.launch

/**
 * 翻译请求的排队与并发。
 *
 * 以前是 `synchronized(serverLock)` 包住整个请求：一次只翻一条，而且**每个排队的请求都在
 * `Dispatchers.IO` 上阻塞等锁**（IO 池上限 64），排队一长就把图片 / 网络线程占满。现在：
 *
 * - **不占线程**：等并发位 / 等结果都靠 `CompletableDeferred.await()` 挂起，没有阻塞的锁；
 * - **并发上限** [parallelism]；
 * - **新的优先**：还没开跑的请求排在队首，最近划到屏幕上的卡片先翻；
 * - **跟着界面走**：调用方的协程被取消（卡片 / 评论离开组合）时，还没派发的排队项直接从队列里拿掉；
 *   **已经派发的那条照常跑完**，并通过 [onDone] 回调交给调用方（写缓存），不因为界面走了就丢掉；
 * - 同一个 [key] 的多个请求共用一份工作（去重），谁先来谁登记 [onDone]。
 *
 * 服务器启停不归它管（在 [feedhub.translate.RustTranslateEngine] 里，只有一份、互斥）：这里只保证同时最多
 * [parallelism] 个请求在跑，重启时不会打断正在跑的请求。
 *
 * **这一层现在的分工**：队列状态（谁在排队、谁在跑、队首是谁、同键去重、取消规则）全在
 * `feedhub-core/src/translate/queue.rs`，这边只做两件宿主的事——**协程挂起 / 派发**和
 * **把队列编号对回 `CompletableDeferred`**。`Queue::pump()` 一次返回"现在该跑哪几个"，
 * 这边给每个起一个协程；跑完 `finish`、调用方走了 `abandon`。
 *
 * [run] 是真正干活的函数（打 llama-server）；测试里换成假引擎。
 */
class TranslationScheduler(
    val parallelism: Int,
    private val scope: CoroutineScope,
    private val run: suspend (text: String, target: String) -> String,
) {
    private val queue = RustTranslateQueue(parallelism)

    /** 只保护 [items] 和"队列编号 ↔ 等待者"的对应关系；队列本身有自己的锁（在 Rust 里）。 */
    private val lock = Any()

    private val items = HashMap<Long, Item>()

    private class Item(val id: Long, val key: String) {
        val result = CompletableDeferred<String>()
        var onDone: ((Result<String>) -> Unit)? = null
    }

    /**
     * 请求翻译 [text]（去重键 [key]）并等结果。[onDone] 只由**第一个**发起这个 key 的调用方登记，
     * 请求真正结束时（翻好 / 失败 / 排队中被取消）一定会被调一次——即使登记的调用方已经被取消。
     */
    suspend fun submit(
        key: String,
        text: String,
        target: String,
        onDone: ((Result<String>) -> Unit)? = null,
    ): String {
        val item: Item
        var created = false
        synchronized(lock) {
            val submit = queue.submit(key, text, target)
            val existing = items[submit.id]
            if (existing != null) {
                item = existing
            } else {
                item = Item(submit.id, key).also {
                    it.onDone = onDone
                    items[submit.id] = it
                }
                created = submit.created
            }
        }
        if (created) pump()
        try {
            return item.result.await()
        } catch (e: CancellationException) {
            abandon(item)
            throw e
        }
    }

    /** 进程退出 / 关翻译：把还排着队的全部取消（已经在跑的不动，让它们跑完）。 */
    fun cancelPending(reason: String = "翻译已关闭") {
        val dropped = synchronized(lock) {
            queue.cancelPending().mapNotNull { id -> items.remove(id) }
        }
        for (item in dropped) {
            val e = CancellationException(reason)
            item.result.cancel(e)
            runCatching { item.onDone?.invoke(Result.failure(e)) }
        }
    }

    /** 补位：把队列里最近、且还在并发上限内的请求派发出去。 */
    private fun pump() {
        for (job in queue.pump()) {
            scope.launch {
                val result = try {
                    Result.success(run(job.text, job.target))
                } catch (e: CancellationException) {
                    complete(job, Result.failure(e))
                    throw e
                } catch (e: Throwable) {
                    Result.failure(e)
                }
                complete(job, result)
            }
        }
    }

    private fun complete(job: TranslateJob, result: Result<String>) {
        val item: Item
        synchronized(lock) {
            queue.finish(job.id)
            item = items.remove(job.id) ?: return
        }
        if (result.isSuccess) {
            item.result.complete(result.getOrThrow())
        } else {
            item.result.completeExceptionally(result.exceptionOrNull() ?: RuntimeException("翻译失败"))
        }
        // onDone 在调度器自己的协程里跑：登记的调用方被取消了也照样写缓存
        runCatching { item.onDone?.invoke(result) }
        pump()
    }

    /** 调用方在"还没派发"时走了：把排队项拿掉；已经派发的就让它跑完（结果仍会走 [Item.onDone]）。 */
    private fun abandon(item: Item) {
        val dropped = synchronized(lock) {
            if (queue.abandon(item.id)) {
                items.remove(item.id)
                true
            } else {
                false
            }
        }
        if (dropped) {
            val e = CancellationException("排队中的翻译被取消")
            item.result.cancel(e)
            runCatching { item.onDone?.invoke(Result.failure(e)) }
        }
    }
}
