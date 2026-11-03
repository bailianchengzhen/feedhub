//! 富文本的翻译（`app/.../detail/TranslatedRichText.kt` + `Comments.kt` 里那两个扩展函数）。
//!
//! 只翻**段落 / 小标题 / 引用**里的文字；图片 / 视频 / 卡片 / 代码块 / 分割线原样保留
//!
//! 两件事：
//! - [`plan`]：这段富文本里**哪几块要翻**、每块要翻的原文是什么（块和文字一一对应，
//!   不可翻的块给 `None`）。宿主拿它去 `ensure()` 每一条；
//! - [`apply`]：把译好的文字**拼回**同样的块结构（块类型保留，行内链接 / 表情 / 粗体在译文视图里先丢，
//!   切回原文就有——原来的行为）。
//!
//! 为什么是"一次一批"而不是一块一次：一篇长正文有几十块，界面上每次重组都要过一遍；
//! 一块一次过 FFI 就是几十次 JNA 调用（量过，逐条 1 ms 级）。

use crate::detail::{Block, RichText, Span};
use crate::hosted;
use crate::translate::lang;

/// 一块里真正要翻的文字；代码块、图片、卡片、视频、分割线不翻。
///
/// Kotlin 原文：
/// ```text
/// is Block.Paragraph -> spans.joinToString("") { it.text }.trim().takeIf { it.isNotBlank() }
/// ```
pub fn translatable_text(block: &Block) -> Option<String> {
    let spans = match block {
        Block::Paragraph { spans } => spans,
        Block::Heading { spans } => spans,
        Block::Quote { spans } => spans,
        _ => return None,
    };
    let joined: String = spans.iter().map(|s| s.text()).collect();
    let trimmed = lang::trim(&joined);
    if hosted::is_kotlin_blank(trimmed) {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// 保留块类型，只把文字换成译文（Kotlin `Block.withTranslatedText`）。
///
/// 不可翻的块原样返回（调用方一般也不会传）。
pub fn with_translated_text(block: &Block, text: &str) -> Block {
    match block {
        Block::Paragraph { .. } => Block::Paragraph {
            spans: vec![Span::Text {
                text: text.to_string(),
                bold: false,
                italic: false,
            }],
        },
        Block::Heading { .. } => Block::Heading {
            spans: vec![Span::Text {
                text: text.to_string(),
                bold: false,
                italic: false,
            }],
        },
        Block::Quote { .. } => Block::Quote {
            spans: vec![Span::Text {
                text: text.to_string(),
                bold: false,
                italic: false,
            }],
        },
        other => other.clone(),
    }
}

/// 整段富文本里所有可翻的块（顺序和 [`RichText::blocks`] 一致）。
pub fn translatable_texts(body: &RichText) -> Vec<String> {
    body.blocks
        .iter()
        .filter_map(translatable_text)
        .collect()
}

/// 每一块要翻的原文（不可翻的给 `None`）。宿主拿它下标对齐 [`apply`]。
pub fn plan(blocks: &[Block]) -> Vec<Option<String>> {
    blocks.iter().map(translatable_text).collect()
}

/// 把译好的文字拼回富文本：`texts[i]` 是第 `i` 块的译文（`None` = 没译 / 不翻 / 越界）。
///
/// 长度不匹配时按较短的那一边算（多出来的块原样保留、多出来的译文忽略），
/// 宿主不用担心两个列表在重组之间错位。
pub fn apply(blocks: &[Block], texts: &[Option<String>]) -> Vec<Block> {
    blocks
        .iter()
        .enumerate()
        .map(|(i, block)| match texts.get(i).and_then(|t| t.as_deref()) {
            Some(text) if translatable_text(block).is_some() => with_translated_text(block, text),
            _ => block.clone(),
        })
        .collect()
}

/// [`apply`] 的 `RichText` 版本。
pub fn apply_body(body: &RichText, texts: &[Option<String>]) -> RichText {
    RichText {
        blocks: apply(&body.blocks, texts),
    }
}

