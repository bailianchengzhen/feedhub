package feedhub.core.sync

/**
 * 收藏 / 历史同步的**合并引擎**（纯函数，不碰数据库 / 网络 / 界面，方便离线测试）。
 *
 * 墓碑（`deleted = true`）也参与比较；
 *   墓碑比"加回来"旧时允许复活——用户重新收藏 / 重新打开一次就会产生更新的 HLC。
 * - 删除收藏夹（不删条目）时，导出方已经给夹内每条写了新 HLC、`folderUid = null`；删条目时每条都是墓碑。
 *   引擎只做逐条 LWW，不猜"收藏夹没了条目该去哪"。
 * - 收藏夹用稳定 uid；**首次同步两边各有一个同名收藏夹**时按名字认领：合并结果里只留一个（uid 由
 *   `(revHlc, revDev, uid)` 最大者胜，确定且与方向无关），其余 uid 通过 [MergeOutcome.folderAliases] 指向它。
 * - 历史：访问时间和播放进度分开比（`revHlc` / `progressHlc`）。墓碑行不留进度。
 * - "清空历史"是全局标记，取两端最大值；`max(revHlc, progressHlc) <= cleared` 的历史行直接丢掉
 *   （不导出、不写入），清空之后的新访问（HLC 更大）照常保留。
 * - 保留期（"只保留最近 N 天"）是**本机策略**：只在导入时把远端太老的历史过滤掉，不写墓碑、不传播。
 *
 * 交换律 / 幂等：合并结果只依赖两边记录的集合，不依赖顺序；同名收藏夹的 uid 选择是确定的。
 */
object LibraryMerge {

    /**
     * 合并两边，返回合并后的状态和相对 [local] 的改动计数。**实现在 Rust**
     * （`feedhub-core/src/sync/merge.rs`，逐行对着这份 Kotlin 原实现写）。
     *
     * 为什么搬：合并规则是同步的正确性核心（LWW / 墓碑 / 清空标记 / 同名认领 / 计数），
     * 而且要么全对要么全错——两份实现漂一点点，两台设备就会"合完还不一致"。
     * Kotlin 原实现在 `core` 的 testFixtures（`feedhub.core.sync.legacy.LegacyLibraryMerge`），
     * 只当对拍参照（`SyncMergeDualRunTest`：≥200 组固定种子随机快照对，逐字段比）。
     */
    fun merge(local: SyncState, remote: SyncState, retentionCutoffMs: Long? = null): MergeOutcome =
        feedhub.rust.RustSync.merge(local, remote, retentionCutoffMs)

    /**
     * 把本机状态按同名认领的映射改一下（只用于计数比较）。
     *
     * 这一条留在 Kotlin：它只在 [merge] 内部用，而 Rust 那边已经算好 `counts` 了；
     * 保留是因为 `RustLibrary` 的旧调用方还在用它。
     */
    fun remapLocal(local: SyncState, aliases: Map<String, String>): SyncState {
        if (aliases.isEmpty()) return local
        return local.copy(
            folders = local.folders.map { row -> aliases[row.uid]?.let { row.copy(uid = it) } ?: row },
            favorites = local.favorites.map { row ->
                val mapped = row.folderUid?.let { aliases[it] ?: it }
                if (mapped == row.folderUid) row else row.copy(folderUid = mapped)
            },
        )
    }
}
