//! 正文与评论图片排布的纯函数（`ImageLayout.kt` + `CommentImages.kt`）。
//!
//! 包含宫格分组、列数、格数、多余计数、单图按比例与封顶高度排版、评论缩略图规则。

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BodyImageBox {
    pub height: f32,
    pub cropped: bool,
    pub fit_width: bool,
}

impl BodyImageBox {
    pub fn width_for(&self, container_width: f32, ratio: f32) -> f32 {
        if !self.fit_width || container_width <= 0.0 {
            container_width
        } else {
            (self.height * ratio).min(container_width)
        }
    }
}

pub const MAX_GRID_CELLS: i32 = 9;
pub const MIN_BODY_IMAGE_RATIO: f32 = 0.5;
pub const MAX_BODY_IMAGE_RATIO: f32 = 3.0;
pub const DEFAULT_BODY_IMAGE_RATIO: f32 = 4.0 / 3.0;
pub const COMMENT_THUMB_MAX: i32 = 4;

/// 宫格列数：1 张走单图，2-4 张两列，5 张以上三列。
pub fn image_grid_columns(count: i32) -> i32 {
    if count <= 1 {
        1
    } else if count <= 4 {
        2
    } else {
        3
    }
}

/// 宫格实际画几个格子（最多 9）。
pub fn image_grid_cells(count: i32) -> i32 {
    count.min(MAX_GRID_CELLS)
}

/// 第 9 格上要叠的 "+N"：没超出就是 0。
pub fn image_grid_more(count: i32) -> i32 {
    (count - MAX_GRID_CELLS).max(0)
}

/// 正文里单张图按宽度 [width] 排出来多高。
pub fn body_image_height(width: f32, ratio: f32, max_height: f32) -> BodyImageBox {
    let safe_ratio = if ratio.is_finite() && ratio > 0.0 {
        ratio.clamp(MIN_BODY_IMAGE_RATIO, MAX_BODY_IMAGE_RATIO)
    } else {
        DEFAULT_BODY_IMAGE_RATIO
    };
    if width <= 0.0 {
        return BodyImageBox {
            height: 0.0,
            cropped: false,
            fit_width: false,
        };
    }
    let natural = width / safe_ratio;
    // 横图和方图（比例 >= 1）的高天然不超过容器宽，不会吃掉一屏，也就不封顶。
    // 只有竖图才用 max_height 封顶。
    let cap = if safe_ratio >= 1.0 {
        f32::MAX
    } else {
        max_height
    };
    if natural > cap + 0.5 {
        BodyImageBox {
            height: max_height,
            cropped: true,
            fit_width: true,
        }
    } else {
        BodyImageBox {
            height: natural,
            cropped: false,
            fit_width: false,
        }
    }
}

/// 单张图（不是宫格）在给定容器宽度里的排布。
pub fn fitted_image_box(width: f32, ratio: f32, max_height: f32) -> BodyImageBox {
    body_image_height(width, ratio, max_height)
}

/// 批量计算单图高度排版。
pub fn body_image_height_batch(inputs: &[(f32, f32, f32)]) -> Vec<BodyImageBox> {
    inputs.iter().map(|&(w, r, m)| body_image_height(w, r, m)).collect()
}

/// 图片块声明的宽高比；没给（或给了 0 / 负数）就是 None。
pub fn declared_ratio(width: Option<i32>, height: Option<i32>) -> Option<f32> {
    let w = width?;
    let h = height?;
    if w <= 0 || h <= 0 {
        return None;
    }
    Some(w as f32 / h as f32)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RichUnitRange {
    pub first_index: usize,
    pub len: usize,
}

/// 正文块分组区间：连续的图片（≥2 张）收成一组画宫格，别的块各算一个；1 张图照旧单独一块。
pub fn rich_units(is_image_block: &[bool]) -> Vec<RichUnitRange> {
    let n = is_image_block.len();
    let mut out = Vec::with_capacity(n);
    let mut i = 0;
    while i < n {
        if is_image_block[i] {
            let mut j = i;
            while j < n && is_image_block[j] {
                j += 1;
            }
            if j - i >= 2 {
                out.push(RichUnitRange {
                    first_index: i,
                    len: j - i,
                });
                i = j;
                continue;
            }
        }
        out.push(RichUnitRange {
            first_index: i,
            len: 1,
        });
        i += 1;
    }
    out
}

/// 预览框里一行画几张（≤ 4）。
pub fn comment_thumb_cells(count: i32) -> i32 {
    count.clamp(0, COMMENT_THUMB_MAX)
}

/// 预览框里没画出来的张数。
pub fn comment_thumb_more(count: i32) -> i32 {
    (count - COMMENT_THUMB_MAX).max(0)
}

/// 评论里的图片请求地址：走缩略图改写规则，宽度夹取在 200..1920。
pub fn comment_image_url(url: &str, width_px: i32) -> String {
    crate::thumb::of(url, width_px.clamp(200, 1920))
}

/// 批量改写评论图片请求地址。
pub fn comment_image_url_batch(items: &[(&str, i32)]) -> Vec<String> {
    items.iter().map(|(u, w)| comment_image_url(u, *w)).collect()
}

