//! UniFFI exports for present images layout and comment images.

use feedhub_core::present::images as core;

#[derive(Debug, Clone, Copy, uniffi::Record)]
pub struct FfiPresentBodyImageBox {
    pub height: f32,
    pub cropped: bool,
    pub fit_width: bool,
}

impl From<core::BodyImageBox> for FfiPresentBodyImageBox {
    fn from(b: core::BodyImageBox) -> Self {
        Self {
            height: b.height,
            cropped: b.cropped,
            fit_width: b.fit_width,
        }
    }
}

#[derive(Debug, Clone, Copy, uniffi::Record)]
pub struct FfiPresentRichUnitRange {
    pub first_index: u32,
    pub len: u32,
}

#[uniffi::export]
pub fn present_image_grid_columns(count: i32) -> i32 {
    core::image_grid_columns(count)
}

#[uniffi::export]
pub fn present_image_grid_cells(count: i32) -> i32 {
    core::image_grid_cells(count)
}

#[uniffi::export]
pub fn present_image_grid_more(count: i32) -> i32 {
    core::image_grid_more(count)
}

#[uniffi::export]
pub fn present_body_image_height(width: f32, ratio: f32, max_height: f32) -> FfiPresentBodyImageBox {
    core::body_image_height(width, ratio, max_height).into()
}

#[uniffi::export]
pub fn present_body_image_height_batch(
    widths: Vec<f32>,
    ratios: Vec<f32>,
    max_heights: Vec<f32>,
) -> Vec<FfiPresentBodyImageBox> {
    let n = widths.len().min(ratios.len()).min(max_heights.len());
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        out.push(core::body_image_height(widths[i], ratios[i], max_heights[i]).into());
    }
    out
}

#[uniffi::export]
pub fn present_body_image_width_for(
    height: f32,
    fit_width: bool,
    container_width: f32,
    ratio: f32,
) -> f32 {
    let box_ = core::BodyImageBox {
        height,
        cropped: false,
        fit_width,
    };
    box_.width_for(container_width, ratio)
}

#[uniffi::export]
pub fn present_declared_ratio(width: Option<i32>, height: Option<i32>) -> Option<f32> {
    core::declared_ratio(width, height)
}

#[uniffi::export]
pub fn present_rich_units(is_image_block: Vec<bool>) -> Vec<FfiPresentRichUnitRange> {
    core::rich_units(&is_image_block)
        .into_iter()
        .map(|r| FfiPresentRichUnitRange {
            first_index: r.first_index as u32,
            len: r.len as u32,
        })
        .collect()
}

#[uniffi::export]
pub fn present_comment_thumb_cells(count: i32) -> i32 {
    core::comment_thumb_cells(count)
}

#[uniffi::export]
pub fn present_comment_thumb_more(count: i32) -> i32 {
    core::comment_thumb_more(count)
}

#[uniffi::export]
pub fn present_comment_image_url(url: String, width_px: i32) -> String {
    core::comment_image_url(&url, width_px)
}

#[uniffi::export]
pub fn present_comment_image_url_batch(urls: Vec<String>, width_pxs: Vec<i32>) -> Vec<String> {
    let n = urls.len().min(width_pxs.len());
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        out.push(core::comment_image_url(&urls[i], width_pxs[i]));
    }
    out
}

