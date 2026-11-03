//! 卡片 / 表情 / 评论图的**缩略图地址改写**的 UniFFI 边界（`FfiThumb*`）。
//!
//! 界面（卡片 / 九宫格预取 / 详情占位）和封面向量
//! **都从这里过**——一份口径，不存在"界面一套、向量一套"。
//!
//! 过边界按**一批**（一屏 40 张卡一次），别一张一次：见 [`ffi_thumb_of_many`] / [`ffi_thumb_prefetch_many`] /
//! [`ffi_thumb_identity_many`]。单条那几个函数留着给调用点确实只有一条的地方（详情占位、封面窗口）。

use std::sync::Arc;

use feedhub_core::thumb as core;

/// 卡片宽度向上取到 160 的倍数（夹在 320～1920）。
#[uniffi::export]
pub fn ffi_thumb_bucket(card_width_px: i32) -> i32 {
    core::bucket(card_width_px)
}

/// 卡片该用的地址（认得的 CDN 换成缩略图，其余原样）。
#[uniffi::export]
pub fn ffi_thumb_of(url: String, width_px: i32) -> String {
    core::of(&url, width_px)
}

/// 一批 [`ffi_thumb_of`]（等长等序；一屏卡片只过一次边界）。
#[uniffi::export]
pub fn ffi_thumb_of_many(urls: Vec<String>, width_px: i32) -> Vec<String> {
    core::of_many(&urls, width_px)
}

/// 这张图能不能拿缩略图**预取**（`None` = 拿不准是不是原图，不取）。
#[uniffi::export]
pub fn ffi_thumb_prefetch(url: String, width_px: i32) -> Option<String> {
    core::thumbnail_for(&url, width_px)
}

/// 一批 [`ffi_thumb_prefetch`]（等长等序；九宫格第 2～9 张一次算完）。
#[uniffi::export]
pub fn ffi_thumb_prefetch_many(urls: Vec<String>, width_px: i32) -> Vec<Option<String>> {
    core::thumbnail_for_many(&urls, width_px)
}

/// 同一张图的"身份"（缩略写法 / 签名 / 主机不同也认得出来）。
#[uniffi::export]
pub fn ffi_thumb_identity(url: String) -> String {
    core::identity(&url)
}

/// 一批 [`ffi_thumb_identity`]（详情页拿卡片缩略图当占位时一屏一次）。
#[uniffi::export]
pub fn ffi_thumb_identity_many(urls: Vec<String>) -> Vec<String> {
    core::identity_many(&urls)
}

/// **同一个地址、好几个宽度档**（去重保序）：封面向量试缓存时用（`CACHE_WIDTHS` 那 8 档）。
#[uniffi::export]
pub fn ffi_thumb_of_widths(url: String, widths: Vec<i32>) -> Vec<String> {
    core::of_widths(&url, &widths)
}

/// 地址看起来已经是服务端缩好的那一档。
#[uniffi::export]
pub fn ffi_thumb_is_server_thumbnail(url: String) -> bool {
    core::is_server_thumbnail(&url)
}

// ===========================================================================
// 对象：一屏卡片的批量入口
// ===========================================================================

/// **一次过边界处理一屏卡片**。
///
/// 为什么要有对象：界面那边是 Compose，每一帧都可能问一遍；逐个地址调 [`ffi_thumb_of_many`] 也行，
/// 但这个对象把"宽度档"记住，调用点只传一屏地址——省掉每次都传参数，
///
/// 它**不缓存结果**：规则本身是纯函数，缓存留给上层的 Compose（`remember`）和 Coil。
#[derive(uniffi::Object)]
pub struct FfiThumbnails {
    width_px: i32,
}

#[uniffi::export]
impl FfiThumbnails {
    /// 按一屏卡片的像素宽度建一个（宽度 → [`core::bucket`] 由构造自己做，调用点不用记得）。
    #[uniffi::constructor]
    pub fn new(card_width_px: i32) -> Arc<Self> {
        Arc::new(FfiThumbnails {
            width_px: core::bucket(card_width_px),
        })
    }

    /// 这一档的宽度（调用点想复用到 Coil 请求尺寸时用）。
    pub fn width_px(&self) -> i32 {
        self.width_px
    }

    /// 一屏卡片的地址 → 该用的地址（等长等序）。
    pub fn of_many(&self, urls: Vec<String>) -> Vec<String> {
        core::of_many(&urls, self.width_px)
    }

    /// 一屏卡片的地址 → 能预取的那些缩略图（等长等序，`None` = 不取）。
    pub fn prefetch_many(&self, urls: Vec<String>) -> Vec<Option<String>> {
        core::thumbnail_for_many(&urls, self.width_px)
    }
}
