package feedhub.core

/**
 * 以后本地模型判"这条要不要藏"的接口。
 *
 * 外出模式按"平台标记 → 关键词和社区黑名单 → 图片分类小模型 → 小语言模型只判拿不准的"逐层来
 * 。现在已经有的两层：
 * 1. 平台自己标的 18+：`feedhub.brain.AdultFilter`（一直生效，和外出模式无关）；
 * 2. 关键词 + 社区：`feedhub.brain.ContentFilter`。
 *
 * 这个接口留给第 3、4 层：图片分类（[Stage.IMAGE]）和小语言模型（[Stage.TEXT]）。以后接的时候：
 * - 只有 `feedhub.brain.ContentFilter` 没有命中（或者平台 / 名单给不出结论）的条目才问它，**越往后越贵**；
 * [Verdict.UNSURE] **外出时默认隐藏**
 * ，外出模式关着时照常显示；
 * - 它可能要读网络（模型下载）或跑本地推理，所以是 `suspend`；实现必须自己限并发，别把一批几十条同时塞给模型。
 *
 * 现在**一个实现都没有**，`feedhub.brain.ContentFilter` 也不调用它——接的时候在 `FeedState` 取回一页的地方加一次调用就行。
 */
interface ContentJudge {
    /** 判一条。实现不许抛异常给调用方（判不了就返回 [Verdict.UNSURE]），也不许改 [item]。 */
    suspend fun judge(item: FeedItem): Verdict

    /** 判的是哪一层（给实现挑模型用，也给日志用）。 */
    enum class Stage {
        /** 图片分类小模型：看卡片封面 / 正文第一张图。 */
        IMAGE,

        /** 小语言模型：看标题 + 摘要，只判前面几层拿不准的。 */
        TEXT,
    }

    /** 判断结果。 */
    enum class Verdict {
        /** 能看，照常显示。 */
        SHOW,

        /** 不看，整条隐藏（不打码、不折叠）。 */
        HIDE,

        /** 拿不准：外出模式开着时隐藏，关着时照常显示。 */
        UNSURE,
    }
}
