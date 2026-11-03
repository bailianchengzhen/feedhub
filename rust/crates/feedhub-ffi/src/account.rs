//! **账号层**：扫码登录 / "我是谁" / 关注的 FFI 形状 + core → FFI 的转换。
//!
//! 三家一起用（B站 / 知乎 / 抖音 / 小黑盒 / 贴吧），所以单独一个文件：
//! - [`FfiQrLogin`] / [`FfiQrState`]：扫码登录（Kotlin `QrLogin` / `QrLoginState`，字段一对一）；
//! - [`FfiAccountFollowPage`]：关注列表的一页（`FfiFollowTarget` 是详情那期就有的类型，直接复用）；
//! - 「我是谁」直接用 [`crate::FfiUserRef`] + `Option`（没登录就是 `None`）。
//!
//! 别家不许再占。

use feedhub_core::account::{FollowPage, QrLogin, QrState};

use crate::{to_ffi_user, FfiFollowTarget, FfiUserRef};

/// 一次扫码登录的二维码（Kotlin `QrLogin`）。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiQrLogin {
    pub platform: String,
    /// 二维码里编码的内容（前端自己画成二维码）。
    pub content: String,
    /// 平台用来查询状态的 key。
    pub token: String,
    /// 平台另外需要记住的东西（贴吧的 `gid`），调用方原样传回。
    pub extra: Vec<FfiQrParam>,
    /// 什么时候过期（Unix 毫秒），不知道就是 `None`。
    pub expires_at_ms: Option<i64>,
    pub poll_interval_ms: i64,
    pub hint: String,
}

/// `extra` 的一项（有序，**不要**换成 Map：贴吧以后可能加别的键，顺序要稳定）。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiQrParam {
    pub name: String,
    pub value: String,
}

/// 一次轮询的结果（Kotlin `QrLoginState`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FfiQrStateKind {
    Waiting,
    Scanned,
    Success,
    Expired,
    Failed,
}

/// 一次轮询的结果 + 附带的账号 / 失败原因。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiQrState {
    pub kind: FfiQrStateKind,
    /// 只有 `Success` 时可能非空（验证接口没给身份时也是 `None`，前端再调 `whoami`）。
    pub user: Option<FfiUserRef>,
    /// 只有 `Failed` 时有：一句给用户看的话（**不含凭据**）。
    pub message: Option<String>,
}

/// 一页关注对象（Kotlin `Page<FollowTarget>`）。
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiAccountFollowPage {
    pub items: Vec<FfiFollowTarget>,
    /// 下一页游标（字符串，调用方原样传回）；`None` = 没有下一页。
    pub next: Option<String>,
}

pub(crate) fn to_ffi_qr_login(login: &QrLogin) -> FfiQrLogin {
    FfiQrLogin {
        platform: login.platform.clone(),
        content: login.content.clone(),
        token: login.token.clone(),
        extra: login
            .extra
            .iter()
            .map(|(name, value)| FfiQrParam {
                name: name.clone(),
                value: value.clone(),
            })
            .collect(),
        expires_at_ms: login.expires_at_ms,
        poll_interval_ms: login.poll_interval_ms,
        hint: login.hint.clone(),
    }
}

/// `FfiQrLogin` → core（轮询时把 Kotlin 拿回来的二维码原样交回去）。
pub(crate) fn from_ffi_qr_login(login: &FfiQrLogin) -> QrLogin {
    QrLogin {
        platform: login.platform.clone(),
        content: login.content.clone(),
        token: login.token.clone(),
        extra: login
            .extra
            .iter()
            .map(|p| (p.name.clone(), p.value.clone()))
            .collect(),
        expires_at_ms: login.expires_at_ms,
        poll_interval_ms: login.poll_interval_ms,
        hint: login.hint.clone(),
    }
}

pub(crate) fn to_ffi_qr_state(state: &QrState) -> FfiQrState {
    match state {
        QrState::Waiting => FfiQrState {
            kind: FfiQrStateKind::Waiting,
            user: None,
            message: None,
        },
        QrState::Scanned => FfiQrState {
            kind: FfiQrStateKind::Scanned,
            user: None,
            message: None,
        },
        QrState::Success(user) => FfiQrState {
            kind: FfiQrStateKind::Success,
            user: user.as_ref().map(to_ffi_user),
            message: None,
        },
        QrState::Expired => FfiQrState {
            kind: FfiQrStateKind::Expired,
            user: None,
            message: None,
        },
        QrState::Failed(message) => FfiQrState {
            kind: FfiQrStateKind::Failed,
            user: None,
            message: Some(message.clone()),
        },
    }
}

pub(crate) fn to_ffi_follow_page(page: &FollowPage) -> FfiAccountFollowPage {
    use feedhub_core::detail::FollowKind;
    FfiAccountFollowPage {
        items: page
            .items
            .iter()
            .map(|t| FfiFollowTarget {
                platform: t.platform.clone(),
                kind: match t.kind {
                    FollowKind::User => crate::FfiFollowKind::User,
                    FollowKind::Forum => crate::FfiFollowKind::Forum,
                },
                id: t.id.clone(),
                name: t.name.clone(),
                avatar: t.avatar.clone(),
                description: t.description.clone(),
                url: t.url.clone(),
            })
            .collect(),
        next: page.next.clone(),
    }
}

pub(crate) fn from_ffi_follow_target(
    target: &FfiFollowTarget,
) -> feedhub_core::detail::FollowTarget {
    feedhub_core::detail::FollowTarget {
        platform: target.platform.clone(),
        kind: match target.kind {
            crate::FfiFollowKind::User => feedhub_core::detail::FollowKind::User,
            crate::FfiFollowKind::Forum => feedhub_core::detail::FollowKind::Forum,
        },
        id: target.id.clone(),
        name: target.name.clone(),
        avatar: target.avatar.clone(),
        description: target.description.clone(),
        url: target.url.clone(),
    }
}
