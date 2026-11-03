package feedhub.app.video

import java.lang.ref.WeakReference

/**
 * 进程里还活着的播放会话。App 关闭时用它**主动**逐个收尾（存进度 + 释放播放器），
 * 不再靠 Compose 的 `onDispose` 时序碰运气——`exitProcess` 会跳过 `onDispose`，正在播的视频
 * 进度就丢了。
 *
 * 弱引用：会话离开界面、没人引用之后自然消失，不用手工清理，也不会把整棵组合树拖着不放。
 * 线程安全：注册（组合线程）/ 收尾可能不在一个线程上。
 */
object ActivePlaybackSessions {
    private val lock = Any()
    private val sessions = ArrayList<WeakReference<PlaybackSession>>()

    fun register(session: PlaybackSession) {
        synchronized(lock) {
            sessions.removeAll { it.get() == null }
            sessions += WeakReference(session)
        }
    }

    fun unregister(session: PlaybackSession) {
        synchronized(lock) { sessions.removeAll { it.get() == null || it.get() === session } }
    }

    /** 给每个还活着的会话收尾（存进度 + 释放播放器）；返回收尾了几个。 */
    fun releaseAll(): Int {
        val alive = synchronized(lock) {
            val alive = sessions.mapNotNull { it.get() }
            sessions.removeAll { it.get() == null || it.get() in alive }
            alive
        }
        for (session in alive) runCatching { session.release() }
        return alive.size
    }

    /** 现在还活着的会话（性能探针读播放器的帧数用）。 */
    internal fun alive(): List<PlaybackSession> = synchronized(lock) { sessions.mapNotNull { it.get() } }

    /** 测试用：清掉登记表，别让别的用例的会话串进来。 */
    internal fun clearForTest() {
        synchronized(lock) { sessions.clear() }
    }
}
