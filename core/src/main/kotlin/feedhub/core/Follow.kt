package feedhub.core

import kotlinx.serialization.Serializable

/**
 * 关注 / 取关：唯一允许的写操作，只有 B站（UP 主）和贴吧（吧）实现。
 *
 * 安全约定：[setFollowing] 只在用户点按钮时调用，Source 内部绝不自动调用、不重试；
 * 写离线测试可以，在线实测必须先征得用户同意、由用户指定对象，测完恢复原状。
 */
interface FollowSource : Source {
    /** 当前账号关注的对象（"我的 → 关注管理"用），分页。没登录抛 [NotLoggedInException]。 */
    suspend fun followings(cursor: Cursor? = null): Page<FollowTarget>

    /** 关注（[follow] = true）或取关。已经是目标状态时不报错。 */
    suspend fun setFollowing(target: FollowTarget, follow: Boolean)
}

@Serializable
enum class FollowKind { USER, FORUM }

/** 可关注的对象。贴吧的吧：[id] 是 fid，[name] 是吧名（关注接口两个都要）。 */
@Serializable
data class FollowTarget(
    val platform: Platform,
    val kind: FollowKind,
    val id: String,
    val name: String,
    val avatar: String? = null,
    /** 签名、吧简介之类的一行说明。 */
    val description: String? = null,
    val url: String? = null,
)

@Serializable
data class FollowState(
    val target: FollowTarget,
    /** 当前是否已关注；没登录或接口没给就是 null。 */
    val following: Boolean? = null,
)
