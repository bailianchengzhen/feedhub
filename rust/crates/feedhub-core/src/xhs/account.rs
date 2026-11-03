//! 小红书的**账号**解析（"我是谁"）：`GET /api/sns/web/v2/user/me` 的 `data` → `UserRef`。
//!
//! 逐条对照 `XhsParser.me`：
//!
//! ```kotlin
//! val d = json.obj("data") ?: return null
//! if (d.bool("guest") == true) return null
//! val id = d.text("user_id") ?: return null
//! return UserRef(P, id, d.text("nickname") ?: d.text("red_id") ?: id,
//!     (d.text("images") ?: d.text("imageb") ?: d.text("image") ?: d.text("avatar"))?.toHttps(), userUrl(id))
//! ```
//!
//! 三条口径（都是 `core/JsonExt.kt` 的语义，Rust 侧在 `crate::json` 里有一份一样的）：
//! - `text()` 是**非空白**字符串（`""` / 全空白都算没有），所以空昵称会退回 `red_id`、再退回 `id`；
//! - `guest == true` 是游客态（没登录），**当作没登录**返回 `None`（不是错误）；
//! - 头像地址过一遍 `toHttps`（小红书给的是 `http://sns-avatar...`）。
//!
//! 这个文件只有解析，没有请求：请求在 [`super::source::XhsHostedSource::whoami`]（它还要判断
//! "a1 + web_session 在不在"、"401 / -100 / -101 折成 None"）。

use crate::json::{self, JsonMap};
use crate::model::UserRef;
use crate::xhs::{self, XHS};

/// `data` → `UserRef`（游客 / 没有 `user_id` 都是 `None`）。
pub fn me_from(root: &JsonMap) -> Option<UserRef> {
    let data = json::obj(root, "data")?;
    if json::bool(data, "guest") == Some(true) {
        return None;
    }
    let id = json::text(data, "user_id")?;
    let name = json::text(data, "nickname")
        .or_else(|| json::text(data, "red_id"))
        .unwrap_or_else(|| id.clone());
    let avatar = json::text(data, "images")
        .or_else(|| json::text(data, "imageb"))
        .or_else(|| json::text(data, "image"))
        .or_else(|| json::text(data, "avatar"))
        .map(|u| json::to_https(&u));
    Some(UserRef {
        platform: XHS.to_string(),
        url: Some(xhs::user_url(&id)),
        id,
        name,
        avatar,
    })
}

