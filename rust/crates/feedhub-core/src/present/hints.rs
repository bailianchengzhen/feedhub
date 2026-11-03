//! **卡片缩略图记录与详情占位规划**。
//!
//! 对应 Kotlin 原版：
//! - `app/src/commonMain/kotlin/feedhub/app/feed/CardImageHints.kt`
//! - `app/src/commonMain/kotlin/feedhub/app/detail/DetailThumbnails.kt`
//!
//! 核心语义与口径：
//! - `CardImageHints`: `MAX_ENTRIES = 500`, `MAX_PER_ITEM = 9`；
//! - 访问序 LRU（命中即刷新位置）；
//! - 去重直接调用内部现成的 [`crate::thumb::identity`]；
//! - 详情图片占位规划 [`detail_image_plan`] 与尺寸占位比例 [`detail_image_ratio`]；
//! - 默认宽高比退回 `4.0 / 3.0`（与 Kotlin `DefaultBodyImageRatio` 逐 bit 相等）。

use std::collections::{HashMap, HashSet, VecDeque};

pub const MAX_ENTRIES: usize = 500;
pub const MAX_PER_ITEM: usize = 9;
pub const DEFAULT_BODY_IMAGE_RATIO: f32 = 4.0_f32 / 3.0_f32;

/// 卡片上那张图当时用的地址与尺寸信息。
#[derive(Debug, Clone, PartialEq)]
pub struct CardImageHint {
    pub source: String,
    pub thumbnail: String,
    pub width: Option<i32>,
    pub height: Option<i32>,
}

impl CardImageHint {
    pub fn new(source: String, thumbnail: String, width: Option<i32>, height: Option<i32>) -> Self {
        CardImageHint {
            source,
            thumbnail,
            width,
            height,
        }
    }

    /// 真实宽高比（宽 / 高）：详情里没有声明宽高时先按它把位置占好；不知道就是 None。
    pub fn ratio(&self) -> Option<f32> {
        let w = self.width?;
        let h = self.height?;
        if w > 0 && h > 0 {
            Some(w as f32 / h as f32)
        } else {
            None
        }
    }
}

/// 媒体信息用于提取尺寸。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MediaImageSpec {
    Image { url: String, width: Option<i32>, height: Option<i32> },
    Video { cover: String, width: Option<i32>, height: Option<i32> },
}

/// `cardImageSize`：查找指定 URL 在媒体列表里的宽高。
pub fn card_image_size(media: &[MediaImageSpec], url: &str) -> (Option<i32>, Option<i32>) {
    for m in media {
        match m {
            MediaImageSpec::Image { url: u, width, height } => {
                if u == url {
                    return (*width, *height);
                }
            }
            MediaImageSpec::Video { cover, width, height } => {
                if cover == url {
                    return (*width, *height);
                }
            }
        }
    }
    (None, None)
}

/// 详情里一张图的显示与占位规划结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetailImagePlan {
    pub url: String,
    pub placeholder: Option<String>,
}

/// 详情里一张图最终画哪个地址、先用什么垫着。
pub fn detail_image_plan(
    url: String,
    original: Option<String>,
    original_images: bool,
    hint: Option<CardImageHint>,
) -> DetailImagePlan {
    let full = if original_images {
        original.unwrap_or(url)
    } else {
        url
    };
    let placeholder = hint.and_then(|h| {
        if h.thumbnail != full {
            Some(h.thumbnail)
        } else {
            None
        }
    });
    DetailImagePlan {
        url: full,
        placeholder,
    }
}

/// 详情里一张图先按什么比例占位。
pub fn detail_image_ratio(declared: Option<f32>, hint: Option<&CardImageHint>) -> f32 {
    if let Some(d) = declared {
        d
    } else if let Some(r) = hint.and_then(|h| h.ratio()) {
        r
    } else {
        DEFAULT_BODY_IMAGE_RATIO
    }
}

/// 在一组 hints 中根据 URL 查找匹配的卡片缩略图。
pub fn find_thumbnail_hint<'a>(hints: &'a [CardImageHint], url: &str) -> Option<&'a CardImageHint> {
    if hints.is_empty() {
        return None;
    }
    // 先看缩略图精确匹配
    if let Some(h) = hints.iter().find(|h| h.thumbnail == url) {
        return Some(h);
    }
    // 再按图片身份（identity）匹配
    let target_id = crate::thumb::identity(url);
    hints.iter().find(|h| crate::thumb::identity(&h.source) == target_id)
}

/// 缩略图缓存池（访问序 LRU，上限 500 条内容）。
#[derive(Debug, Clone)]
pub struct CardImageHintsStore {
    max_entries: usize,
    max_per_item: usize,
    order: VecDeque<String>,
    by_item: HashMap<String, Vec<CardImageHint>>,
}

impl Default for CardImageHintsStore {
    fn default() -> Self {
        Self::new(MAX_ENTRIES, MAX_PER_ITEM)
    }
}

impl CardImageHintsStore {
    pub fn new(max_entries: usize, max_per_item: usize) -> Self {
        CardImageHintsStore {
            max_entries: max_entries.max(1),
            max_per_item: max_per_item.max(1),
            order: VecDeque::new(),
            by_item: HashMap::new(),
        }
    }

    fn touch(&mut self, key: &str) {
        if let Some(pos) = self.order.iter().position(|k| k == key) {
            self.order.remove(pos);
        }
        self.order.push_back(key.to_string());
    }

    fn trim(&mut self) {
        while self.order.len() > self.max_entries {
            if let Some(old_key) = self.order.pop_front() {
                self.by_item.remove(&old_key);
            }
        }
    }

    /// 卡片画过就记下第一张图（置顶，按 identity 去重已有项）。
    pub fn record(&mut self, item_key: &str, hint: CardImageHint) {
        let rest = self.by_item.get(item_key).cloned().unwrap_or_default();
        let hint_id = crate::thumb::identity(&hint.source);
        let kept: Vec<CardImageHint> = rest
            .into_iter()
            .filter(|h| crate::thumb::identity(&h.source) != hint_id)
            .collect();

        let mut list = Vec::with_capacity(1 + kept.len());
        list.push(hint);
        list.extend(kept);
        list.truncate(self.max_per_item);

        self.by_item.insert(item_key.to_string(), list);
        self.touch(item_key);
        self.trim();
    }

    /// 九宫格第 2～9 张：保序追加，按 identity 去重。
    pub fn record_grid(&mut self, item_key: &str, hints: Vec<CardImageHint>) {
        if hints.is_empty() {
            return;
        }
        let old = self.by_item.get(item_key).cloned().unwrap_or_default();
        let mut seen = HashSet::new();
        for h in &old {
            seen.insert(crate::thumb::identity(&h.source));
        }

        let mut to_add = Vec::new();
        for h in hints {
            let id = crate::thumb::identity(&h.source);
            if seen.insert(id) {
                to_add.push(h);
            }
        }

        if to_add.is_empty() {
            if self.by_item.contains_key(item_key) {
                self.touch(item_key);
            }
            return;
        }

        let mut list = old;
        list.extend(to_add);
        list.truncate(self.max_per_item);

        self.by_item.insert(item_key.to_string(), list);
        self.touch(item_key);
        self.trim();
    }

    pub fn of(&mut self, item_key: &str) -> Vec<CardImageHint> {
        if self.by_item.contains_key(item_key) {
            self.touch(item_key);
            self.by_item.get(item_key).cloned().unwrap_or_default()
        } else {
            Vec::new()
        }
    }

    pub fn len(&self) -> usize {
        self.by_item.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_item.is_empty()
    }

    pub fn clear(&mut self) {
        self.by_item.clear();
        self.order.clear();
    }
}

