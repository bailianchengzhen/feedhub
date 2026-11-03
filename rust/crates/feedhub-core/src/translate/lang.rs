//! 语言识别 + 翻译门控（`app/.../translate/LinguaLanguageIdentifier.kt` + `TranslationStore.shouldAutoTranslate`）。
//!
//! ## 模型
//!
//! Kotlin 那边是 `LanguageDetectorBuilder.fromLanguages(Language.CHINESE, Language.ENGLISH).build()`：
//! **只带中文 / 英文两张模型**，所以日语 / 韩语 / 其他语言的文本会因为"两个模型都匹配不上 n-gram"
//! 而回 `UNKNOWN`（Kotlin）→ `null`（`detect`）→ 上层按外语处理（翻）。
//! Rust 这边用同一个作者的 `lingua` crate，`default-features = false` + `features = ["chinese", "english"]`，
//! 只编这两张模型（默认特性会带 75 种语言的模型，`.so` 直接大几十 MB）。
//!
//! ## 对拍（口径必须和 Kotlin 一致）
//!
//! `TranslateLanguageParityTest`（`app/src/desktopTest`）拿 `lingua 1.2.2`（Kotlin）和这条 Rust 实现对拍：
//! 现有测试里的句子 + `app/src/desktopTest/resources/translate/corpus_fixtures.txt`（从全仓 fixture 里
//! 抽出来的 4000 多条真实正文）。**这个判定只关心"是不是中文"这个二值结论**，不关心"具体是哪种外语"。
//!
//! ## 门控规则（和 Kotlin `shouldAutoTranslate` 逐条对齐）
//!
//! 0. 翻译没开 → 不翻；
//! 1. **平台不在「自动翻译点亮的平台」里 → 不翻**；
//! 2. 整条文本去空白后为空 / 一个字母都没有 → 不翻；
//! 3. 平台给了语言（X 的 `legacy.lang`）就信平台（`und` / `zxx` / `qme` / `qht` 不算结论）；
//! 4. 没有平台语言时，短于 [`MIN_AUTO_BYTES`] **字节**（UTF-8）的不自动翻；
//! 5. 其余交给识别器：识别成目标语言就不翻，**识别不了（`None`）按外语处理**。

use std::sync::OnceLock;

use lingua::{Language, LanguageDetector, LanguageDetectorBuilder};

use super::{Detector, Gate};

/// 短于这么多**字节**（UTF-8）的文本不做自动语言判定（照 Chrome 的 25 字节门槛，短文本识别不可靠）。
pub const MIN_AUTO_BYTES: usize = 25;

/// 内置识别器：lingua，只加载中文 / 英文。
///
/// 模型是**懒加载**的（第一次 `detect` 才建 detector + 读模型）；和 Kotlin 那边一样，
/// `LinguaLanguageIdentifier` 的 `by lazy` 也是第一次识别才建。
pub struct Lingua {
    detector: OnceLock<LanguageDetector>,
}

impl Default for Lingua {
    fn default() -> Self {
        Self::new()
    }
}

impl Lingua {
    pub fn new() -> Self {
        Self {
            detector: OnceLock::new(),
        }
    }

    fn detector(&self) -> &LanguageDetector {
        self.detector.get_or_init(|| {
            LanguageDetectorBuilder::from_languages(&[Language::Chinese, Language::English]).build()
        })
    }
}

impl Detector for Lingua {
    fn detect(&self, text: &str) -> Option<String> {
        detect_with(self.detector(), text).map(|s| s.to_string())
    }
}

/// 一次识别：`zh` / `en` / `None`（和 Kotlin `LinguaLanguageIdentifier.detect` 的返回值一一对应）。
pub fn detect(text: &str) -> Option<&'static str> {
    static DETECTOR: OnceLock<Lingua> = OnceLock::new();
    let lingua = DETECTOR.get_or_init(Lingua::new);
    detect_with(lingua.detector(), text)
}

fn detect_with(detector: &LanguageDetector, text: &str) -> Option<&'static str> {
    match detector.detect_language_of(text) {
        Some(Language::Chinese) => Some("zh"),
        Some(Language::English) => Some("en"),
        // UNKNOWN（日语 / 韩语 / 其他，或模型判断不了）：给 None，上层当外语。
        _ => None,
    }
}

/// [`tag`] 是不是目标语言。[`target`] 是设置里的显示名（"中文" / "English"……）。
///
/// Kotlin 原文（`languageMatchesTarget`）：
/// ```text
/// t = tag.trim().lowercase().replace('_','-')
/// if (t.isEmpty() || t == "und" || t == "unknown") return false
/// base = t.substringBefore('-')
/// when (target.trim().lowercase()) {
///   "中文","简体中文","chinese","zh","zh-hans","zh-cn" -> base == "zh" || base == "cmn" || base == "yue"
///   "english","en" -> base == "en"
///   else -> false
/// }
/// ```
pub fn matches_target(tag: &str, target: &str) -> bool {
    let t = trim(tag).to_lowercase().replace('_', "-");
    if t.is_empty() || t == "und" || t == "unknown" {
        return false;
    }
    let base = t.split('-').next().unwrap_or("");
    match trim(target).to_lowercase().as_str() {
        "中文" | "简体中文" | "chinese" | "zh" | "zh-hans" | "zh-cn" => {
            base == "zh" || base == "cmn" || base == "yue"
        }
        "english" | "en" => base == "en",
        _ => false,
    }
}

/// 这条内容要不要**自动**翻译（手动「翻译这条」不走这里）。
///
/// [`detector`] 为 `None` = 这个组装没有本地语言识别（安卓 / 测试 / 截图）：
/// 有平台语言就信平台，没有就**直接翻**（和 Kotlin 的 `identifier?.detect(t) ?: return true` 一致）。
pub fn should_auto_translate(
    text: &str,
    language: Option<&str>,
    platform: Option<&str>,
    gate: &Gate,
    detector: Option<&dyn Detector>,
) -> bool {
    if !gate.enabled {
        return false;
    }
    // 0. 平台门控在语言识别**之前**（没点亮的平台连 lingua 都不碰）。
    if !gate.allows_platform(platform) {
        return false;
    }
    let t = trim(text);
    if t.is_empty() || !t.chars().any(char::is_alphabetic) {
        return false;
    }
    // und / zxx 不是"这条是什么语言"，交给本地识别（X 的纯媒体帖会标 zxx）。
    let platform_language = language
        .map(trim)
        .filter(|s| !s.is_empty())
        .filter(|s| {
            let lower = s.to_lowercase();
            !matches!(lower.as_str(), "und" | "zxx" | "qme" | "qht")
        });
    if let Some(lang) = platform_language {
        return !matches_target(lang, &gate.target);
    }
    if t.as_bytes().len() < MIN_AUTO_BYTES {
        return false;
    }
    match detector.and_then(|d| d.detect(t)) {
        Some(detected) => !matches_target(&detected, &gate.target),
        // 识别不出来（日语 / 韩语 / 其他）：按外语处理；没有识别器时也是这个分支。
        None => true,
    }
}

/// Kotlin `String.trim()`：**真口径**是 `Char.isWhitespace` = `Character.isWhitespace || isSpaceChar`，
/// 比 [`crate::hosted::is_java_whitespace`] 多 NBSP / U+2007 / U+202F 三个字符。
///
/// 没有直接用 `hosted::kotlin_trim`：那个是 Java 口径（`is_java_whitespace`），翻译这一层的对拍
/// 连 NBSP 也要逐字一致，所以单独一份；`kotlin_trim` 是别处（贴吧 / 信息流）在用的口径，动它会改行为。
pub fn trim(s: &str) -> &str {
    let is_ws = |c: char| crate::hosted::is_java_whitespace(c) || matches!(c, '\u{a0}' | '\u{2007}' | '\u{202f}');
    let start = s
        .char_indices()
        .find(|(_, c)| !is_ws(*c))
        .map(|(i, _)| i)
        .unwrap_or(s.len());
    let end = s
        .char_indices()
        .rev()
        .find(|(_, c)| !is_ws(*c))
        .map(|(i, c)| i + c.len_utf8())
        .unwrap_or(start);
    &s[start..end]
}

