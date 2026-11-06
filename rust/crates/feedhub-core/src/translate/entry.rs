//! **翻译入口的判定与状态**。
//!
//! **只在判定为外语时出现**；
//! - 点了变成「正在翻译…」，模型还在加载时是「正在加载翻译模型…」；
//! - 翻好之后就地显示译文 + 一行灰字「由本地模型翻译自 <语言> · 显示原文」，来回切**不重新翻**（走缓存）；
//! - 失败：一行灰字原因 +「重试」；
//! - 中文内容不出现入口；卡片列表（首页 / 关注）不加入口。
//!
//! 这里放的是**界面状态之外的判断**：
//! 1. [`entry_visible`]：这条内容要不要出入口（平台语言优先 → 本地识别 → 判不出来当外语）；
//! 2. [`phase`]：入口现在长什么样（隐藏 / 可点 / 正在加载模型 / 正在翻译 / 有译文 / 失败）；
//! 3. [`display`]：这段文字现在该显示原文还是译文（「显示原文 / 显示译文」那个开关）；
//! 4. [`language_label`]：灰字里那个语言名（`en` → "英语"，认不出的算"外语"）。
//!
//! **和 [`super::lang`] 的区别**：那里是"要不要**自动**翻"（带平台门控：只有点亮的平台才自动翻，
//! 而且短于 25 字节不自动翻）；这里是"要不要给用户一个**手动**入口"——手动翻不看平台、不看那 25 字节
//! （用户点了就是要翻），但依然要判断"这本来就不是中文"，不然每个中文帖子下面都挂一行「翻译」。
//!
//! **判不出来当外语**：和自动翻那条同一个口径（识别成 `None` = 日语 / 韩语 / 或模型认不出，按外语处理）——
//! 宁可多给一个入口，也不要让用户对着一篇日语帖找不到翻译按钮。

use super::lang;
use super::{Detector, Status};

/// 少于这么多**字母**（`char::is_alphabetic`）就不出入口：`666` / `zsbd` / 一个"好"字 / 纯 emoji 都算。
///
/// 比自动翻那 25 **字节**的门槛宽得多：用户主动点的那一下不受"短文本识别不可靠"约束，
/// 这里只排除"一眼就不是一段话"的东西，免得每条点赞评论下面都挂一行「翻译」。
pub const MIN_ENTRY_ALPHA: usize = 2;

/// 入口现在长什么样（Kotlin `TranslationPhase` 一一对应，界面只负责画）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// 不显示（不是外语 / 译文和原文一样 / 翻译关着）。
    Hidden,
    /// 显示一行可点的「翻译」。
    Idle,
    /// 「正在加载翻译模型…」（`llama-server` 还在起 / 还在加载 GGUF）。
    ModelLoading,
    /// 「正在翻译…」（模型就绪，正在跑这一条）。
    Translating,
    /// 有译文了（显示译文 + 灰字那一行）。
    Done,
    /// 失败（灰字原因 +「重试」）。
    Failed,
}

/// [`phase`] 的输入（都是界面现成的布尔量，Rust 不存状态）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Input {
    /// [`entry_visible`] 的结论。
    pub visible: bool,
    /// 翻完了但译文和原文一样（专名 / 缩写 / `zsbd`）——不显示入口，也不显示"已翻译"那一行。
    pub skipped: bool,
    /// 这一条正在翻（排队中或跑着）。
    pub translating: bool,
    /// 已经有译文。
    pub translated: bool,
    /// 上一次失败过（且还没重试）。
    pub failed: bool,
    /// 模型还在加载（`llama-server` 的 `STARTING`）。
    pub model_loading: bool,
}

/// 入口的状态（优先级：隐藏 > 失败 > 有译文 > 在翻 > 可点）。
///
/// 为什么"失败"排在"在翻"前面：失败以后 `translating` 可能还是 true，
/// 这时候显示「重试」比显示「正在翻译…」有用。
pub fn phase(input: Input) -> Phase {
    if !input.visible || input.skipped {
        return Phase::Hidden;
    }
    if input.failed {
        return Phase::Failed;
    }
    if input.translated {
        return Phase::Done;
    }
    if input.translating {
        return if input.model_loading {
            Phase::ModelLoading
        } else {
            Phase::Translating
        };
    }
    Phase::Idle
}

/// 这条内容要不要出翻译入口（用户手动点的那种）。
///
/// 规则和 [`super::lang::should_auto_translate`] 的**语言部分**一致，但：
/// - **不看平台**（手动翻哪家都能用）；
/// - **不看翻译总开关**（关着的时候整个界面都不该有入口，那一层由宿主判断，不在这里重复）；
/// - **没有 25 字节门槛**，只排除"不是一段话"的（见 [`MIN_ENTRY_ALPHA`]）。
///
/// [`detector`] 为 `None` = 这个组装没有本地语言识别：有平台语言就信平台，没有就**不出口**（判不出来
/// 不知道该不该给；宁可少一个入口，也不给中文帖子挂一行「翻译」）。
///
/// 有识别器、但它回 `None`（lingua 只带了中 / 英两张模型，日语 / 韩语 / 其他一律认不出）时**照给**：
/// 认不出就是"不是中文"——X / YouTube 上日语内容很多，判不出来就不给入口等于日语永远翻不了。
/// 这一档和自动翻那条（`should_auto_translate` 的 `None => true`）同一个口径。
pub fn entry_visible(
    text: &str,
    language: Option<&str>,
    target: &str,
    detector: Option<&dyn Detector>,
) -> bool {
    entry_visible_for_sources(text, language, target, &[], detector)
}

/// 带源语言白名单的 [`entry_visible`]：`source_languages` 为空表示任意非目标语言都出入口；
/// 非空（例如 Bergamot 单向英译中传 `["en"]`）时只对匹配的源语言出入口，日/韩等其他语种不出入口。
pub fn entry_visible_for_sources(
    text: &str,
    language: Option<&str>,
    target: &str,
    source_languages: &[String],
    detector: Option<&dyn Detector>,
) -> bool {
    let t = lang::trim(text);
    if t.chars().filter(|c| c.is_alphabetic()).count() < MIN_ENTRY_ALPHA {
        return false;
    }
    let allows = |tag: &str| {
        !lang::matches_target(tag, target)
            && (source_languages.is_empty() || lang::matches_any_lang(tag, source_languages))
    };
    let platform_language = language
        .map(lang::trim)
        .filter(|s| !s.is_empty())
        .filter(|s| {
            let lower = s.to_lowercase();
            !matches!(lower.as_str(), "und" | "zxx" | "qme" | "qht")
        });
    if let Some(lang) = platform_language {
        return allows(lang);
    }
    let Some(detector) = detector else {
        // 没有识别器：只有平台语言能作数，平台也没说就不给入口。
        return false;
    };
    match detector.detect(t) {
        Some(detected) => allows(&detected),
        // 认不出（日语 / 韩语 / 其他）：不限源语言时按外语处理；单语对（如仅 en→zh）不出入口。
        None => source_languages.is_empty(),
    }
}

/// 现在该显示译文吗：`true` = 显示译文，`false` = 显示原文。
///
/// 三种情况都是 `false`：用户点了「显示原文」、没有译文、译文和原文一样（那一档 [`phase`] 已经隐藏入口，
/// 这里再兜一次，免得界面某处漏判时把原文当译文画出来）。
pub fn showing_translation(show_original: bool, translated: Option<&str>, original: &str) -> bool {
    if show_original {
        return false;
    }
    match translated {
        Some(t) => lang::trim(t) != lang::trim(original),
        None => false,
    }
}

/// 和 [`showing_translation`] 配套：现在要画哪一份文字（原文 / 译文）。
pub fn display<'a>(original: &'a str, translated: Option<&'a str>, show_original: bool) -> &'a str {
    if showing_translation(show_original, translated, original) {
        translated.unwrap_or(original)
    } else {
        original
    }
}

/// 富文本版的 [`showing_translation`]：`texts[i]` 是第 `i` 块的译文（`None` = 这块不翻 / 没译）。
///
/// `true` = 现在画**译文**（至少有一块真翻好了，而且用户没点「显示原文」）。
/// 逐块比"译文和原文一样"没有意义（拆出来的块是句子片段），所以这里只要求"有任意一块翻好了"——
/// 单段文字那条（[`showing_translation`]）才需要比内容，因为那里"译文 == 原文"意味着根本没翻。
pub fn showing_blocks(show_original: bool, texts: &[Option<String>]) -> bool {
    if show_original {
        return false;
    }
    texts.iter().any(|t| t.as_deref().is_some_and(|s| !lang::trim(s).is_empty()))
}

/// 富文本版 [`display`]：`texts[i]` 是第 `i` 块的译文；现在该画译文就给翻译过的文本列表，否则原样。
///
/// 返回 `None` = 画原文（调用方直接用 `blocks`）。
pub fn display_blocks<'a>(
    texts: &'a [Option<String>],
    show_original: bool,
) -> Option<&'a [Option<String>]> {
    if showing_blocks(show_original, texts) {
        Some(texts)
    } else {
        None
    }
}

/// 灰字里那个语言名（`由本地模型翻译自 英语 · 显示原文`）。
///
/// 认不出的（`None` / `und` / 没见过的语言）写"外语"——总比写"未知"像人话，也不会把一个日语帖写成"中文"。
pub fn language_label(tag: Option<&str>) -> &'static str {
    let Some(tag) = tag else { return "外语" };
    let t = lang::trim(tag).to_lowercase().replace('_', "-");
    let base = t.split('-').next().unwrap_or("");
    match base {
        "zh" | "cmn" | "yue" | "zh-hans" | "zh-hant" => "中文",
        "en" => "英语",
        "ja" => "日语",
        "ko" => "韩语",
        "fr" => "法语",
        "de" => "德语",
        "es" => "西班牙语",
        "ru" => "俄语",
        "pt" => "葡萄牙语",
        "it" => "意大利语",
        "ar" => "阿拉伯语",
        "th" => "泰语",
        "vi" => "越南语",
        "id" | "ms" => "印尼语",
        "tr" => "土耳其语",
        "nl" => "荷兰语",
        "pl" => "波兰语",
        "hi" => "印地语",
        _ => "外语",
    }
}

/// 引擎状态 → "模型还在加载"（界面据此把「正在翻译…」换成「正在加载翻译模型…」）。
pub fn model_loading(status: Status) -> bool {
    matches!(status, Status::Starting)
}

