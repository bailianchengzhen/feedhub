package feedhub.app.detail

import feedhub.core.Comment

/**
 * 截图工具（`desktopMain/.../dev/Screenshot.kt` 的 `open=` 参数）用的钩子：截图只能截初始状态和滚动后的状态，
 * 楼中楼面板、大图查看层要"点一下"才出现，工具就通过这里直接调 [DetailHost] 的方法。正常界面不用它。
 */
object DetailDebug {
    @Volatile internal var host: DetailHost? = null
    @Volatile internal var comments: () -> List<Comment> = { emptyList() }
    @Volatile internal var bodyImages: () -> List<ViewerImage> = { emptyList() }
    @Volatile internal var poster: Poster = Poster(null)
    @Volatile internal var contentId: String = ""

    /** 打开楼中楼面板：[commentId] 为 null 就挑第一条有回复的评论；没有可打开的返回 false。 */
    fun openReplies(commentId: String?): Boolean {
        val h = host ?: return false
        val all = comments()
        val c = all.firstOrNull { commentId != null && it.id == commentId }
            ?: all.firstOrNull { commentId == null && ((it.replyCount ?: 0) > 0 || it.previewReplies.isNotEmpty()) }
            ?: return false
        h.openReplies(c, poster, contentId)
        return true
    }

    /** 打开大图查看层：正文里的第 [index] 张（从 0 数）。 */
    fun openImage(index: Int): Boolean {
        val h = host ?: return false
        val imgs = bodyImages()
        if (imgs.isEmpty()) return false
        h.openImages(imgs, index)
        return true
    }

    /**
     * 截图工具用：把大图查看器的底部菜单（保存到相册 / 分享图片 / 复制图片链接）摆出来（没开查看器就先开第 [index] 张）。
     * 菜单平时只对手指长按出现（桌面长按不弹，桌面行为保持原样），所以截图只能从这里开；
     * [preview] = true 时把安卓那一套三项都画出来（桌面上「保存 / 分享」其实不支持）。
     */
    fun openImageMenu(index: Int = 0, preview: Boolean = false): Boolean {
        val h = host ?: return false
        if (h.viewer == null && !openImage(index)) return false
        // 查看器是下一次组合才画出来的，菜单由它自己在这个标记上打开（见 ImageViewer 的 ViewerContent）
        pendingImageMenu = true
        pendingImageMenuPreview = preview
        return true
    }

    /**
     * 截图工具用：图文详情**一直按"详情还在加载"的样子画**（不切到正文）。截图只能截到稳定状态，
     * 而"刚进来先垫缩略图"那一屏只有几十毫秒；拿它截那一屏看效果。正常界面永远是 false。
     */
    @Volatile internal var holdBodyLoading: Boolean = false

    /**
     * 截图工具用：详情里的图片**只画占位那一层**（卡片上已经下好的缩略图），不去取原图。
     * 截"刚进来有点糊、过会就好"那种样子用；正常界面永远是 false。
     */
    @Volatile internal var holdFullImages: Boolean = false

    /** 待打开的大图查看器底部菜单（[openImageMenu] 设，查看器组合时消费）。 */
    @Volatile internal var pendingImageMenu: Boolean = false
    @Volatile internal var pendingImageMenuPreview: Boolean = false

    /** 让 key 对应的行内视频"开播"（和点了另一个视频的封面一样：当前在播的那个会停回封面）。用来验证同一时间只播一个。 */
    fun startInlineVideo(key: String): Boolean {
        val h = host ?: return false
        h.videos.start(key)
        return true
    }

    // ---------- 进详情的时间线----------

    /**
     * 截图工具 `play=timeline:<名字>:<毫秒>…` 用：把**这一帧详情页里现在是什么**打印成一行
     * （正文到没到、正文有几块、垫着的那一截还在不在、评论几条、图片成功加载了几张、列表第一个可见条目是谁）。
     *
     * 修"点进详情页每次都要渲染一下"时靠它把"先看到什么、多久之后变成什么"变成可比较的文字；正常界面永远是 null。
     */
    @Volatile internal var timelineProbe: (() -> String)? = null

    /** 详情里的图（[DetailPhoto]）**成功画出原图**的次数，从进详情那一刻开始数（时间线用来说"图什么时候到齐"）。 */
    @Volatile internal var imagesLoaded: Int = 0

    /**
     * 详情里一共有多少张图（正文 / 宫格 / 封面 / 评论）——和 [imagesLoaded] 一起看"图到齐了没有"。
     * 由 [DetailPhoto] 自己数：每块图组合时 +1，离开组合 -1（同一张图重组不会重复数）。
     */
    @Volatile internal var imagesTotal: Int = 0

    internal fun resetImageCounters() {
        imagesLoaded = 0
        imagesTotal = 0
    }

    /** 时间线里那一行"图"，形如 `图片 2/3`。 */
    internal fun imageStatus(): String = "图片 $imagesLoaded/$imagesTotal"

    /** 按一下 Esc 的效果（关大图 / 楼中楼；都没开就返回 false）。 */
    fun dismiss(): Boolean = host?.dismissTop() ?: false

    // ---------- 沉浸播放（竖屏视频）----------

    /** 沉浸界面里开关评论面板（[feedhub.app.video.ImmersionScreen] 注册）。 */
    @Volatile internal var immersionComments: ((Boolean) -> Boolean)? = null

    /** 沉浸界面里开关音量滑块（音量按钮点一下的效果）。 */
    @Volatile internal var immersionVolume: ((Boolean) -> Boolean)? = null

    /** 沉浸界面里点一下"关注"按钮（已关注时 = 弹取关确认框）。 */
    @Volatile internal var immersionUnfollow: (() -> Boolean)? = null

    /** 沉浸界面现在是什么状态（截图工具 `print:immersion` 打一行）。 */
    @Volatile internal var immersionInfo: (() -> String)? = null

    /** 沉浸界面里上下切一条（+1 = 下一条 / -1 = 上一条），截图工具 `open=immersion-next` 用。 */
    @Volatile internal var immersionStep: ((Int) -> Boolean)? = null

    /** 左栏现在滚到哪、选中谁（截图工具 `print:feed`；双栏时看"左栏跟着选中"）。 */
    @Volatile internal var feedGridInfo: (() -> String)? = null

    /** 点开沉浸界面里的评论面板；没在沉浸界面里返回 false。 */
    fun openImmersionComments(): Boolean = immersionComments?.invoke(true) ?: false

    /** 点开沉浸界面里的音量滑块；没在沉浸界面里返回 false。 */
    fun openImmersionVolume(): Boolean = immersionVolume?.invoke(true) ?: false

    /** 点一下沉浸界面里的"关注"按钮（用来截取关的确认框）；没在沉浸界面里 / 平台不支持关注返回 false。 */
    fun clickImmersionFollow(): Boolean = immersionUnfollow?.invoke() ?: false

    /** 上下滑一下（+1 = 下一条），和手指 / 滚轮 / ↑↓ 走同一条路；没在沉浸界面里返回 false。 */
    fun stepImmersion(delta: Int): Boolean = immersionStep?.invoke(delta) ?: false

    // ---------- 「X 式」翻译入口----------

    /**
     * 详情页那一行翻译入口的钩子（截图工具 `open=translate|translate-toggle|translate-retry` 用）。
     *
     * 为什么不用真的点：这一行是正文 / 简介下面的一行小字，坐标随正文长度变（中文 / 英文、折叠 / 展开都不同），
     * 截图工具按坐标点会点到别处；而这里要验的是"点了之后界面变成什么"，所以直接调那一行的回调
     * （和 `open=replies` / `open=image` 同一个套路，正常界面永远不读它）。
     */
    @Volatile internal var translateEntry: (() -> Pair<() -> Unit, () -> Unit>?)? = null

    /** 点一下正文下面那一行「翻译」（= 用户点了入口）。返回 false = 这一条根本没有入口。 */
    fun startTranslation(): Boolean {
        val callbacks = translateEntry?.invoke() ?: return false
        callbacks.first()
        return true
    }

    /** 点一下「显示原文 / 显示译文」（只切显示，不重新翻）。 */
    fun toggleTranslation(): Boolean {
        val callbacks = translateEntry?.invoke() ?: return false
        callbacks.second()
        return true
    }

    /** 当前那一行入口的心情（截图工具打印一行核对用）。 */
    @Volatile internal var translatePhase: (() -> String)? = null
}
