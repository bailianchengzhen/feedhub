//! UniFFI 导出：分享链接清洗、分享文本拼装与外部打开决策。

use feedhub_core::present::links as core;

/// `perform_external_open` 的最终执行结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FfiExternalOpenResult {
    OpenedApp,
    OpenedWeb,
    Copied,
    Failed,
}

impl From<core::ExternalOpenResult> for FfiExternalOpenResult {
    fn from(r: core::ExternalOpenResult) -> Self {
        match r {
            core::ExternalOpenResult::OpenedApp => Self::OpenedApp,
            core::ExternalOpenResult::OpenedWeb => Self::OpenedWeb,
            core::ExternalOpenResult::Copied => Self::Copied,
            core::ExternalOpenResult::Failed => Self::Failed,
        }
    }
}

/// 外部打开执行过程中的阶段。
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FfiExternalOpenStage {
    Initial,
    AppFailed,
    WebFailed,
}

impl From<FfiExternalOpenStage> for core::ExternalOpenStage {
    fn from(s: FfiExternalOpenStage) -> Self {
        match s {
            FfiExternalOpenStage::Initial => Self::Initial,
            FfiExternalOpenStage::AppFailed => Self::AppFailed,
            FfiExternalOpenStage::WebFailed => Self::WebFailed,
        }
    }
}

/// 外部打开的目标与标签。
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiExternalOpen {
    pub deep_link: Option<String>,
    pub web_url: Option<String>,
    pub label: String,
}

impl From<core::ExternalOpen> for FfiExternalOpen {
    fn from(o: core::ExternalOpen) -> Self {
        Self {
            deep_link: o.deep_link,
            web_url: o.web_url,
            label: o.label,
        }
    }
}

/// 外部打开状态机推导出的下一步动作。
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Enum)]
pub enum FfiExternalOpenStep {
    OpenApp { target: String },
    OpenWeb { target: String },
    CopyWeb { target: String },
    Completed { result: FfiExternalOpenResult },
}

impl From<core::ExternalOpenStep> for FfiExternalOpenStep {
    fn from(s: core::ExternalOpenStep) -> Self {
        match s {
            core::ExternalOpenStep::OpenApp { target } => Self::OpenApp { target },
            core::ExternalOpenStep::OpenWeb { target } => Self::OpenWeb { target },
            core::ExternalOpenStep::CopyWeb { target } => Self::CopyWeb { target },
            core::ExternalOpenStep::Completed { result } => Self::Completed {
                result: result.into(),
            },
        }
    }
}

/// 清洗分享链接里的跟踪参数。
#[uniffi::export]
pub fn present_clean_share_url(url: String) -> String {
    core::clean_share_url(&url)
}

/// 复制到剪贴板的分享文字：标题一行、链接一行。
#[uniffi::export]
pub fn present_share_text(title: Option<String>, url: String) -> String {
    core::share_text(title.as_deref(), &url)
}

/// 决策外部打开的候选链接与按钮标签。
#[uniffi::export]
pub fn present_plan_external_open(
    deep_link: Option<String>,
    web_url: Option<String>,
    platform_label: String,
) -> Option<FfiExternalOpen> {
    core::plan_external_open(deep_link.as_deref(), web_url.as_deref(), &platform_label)
        .map(Into::into)
}

/// 根据当前阶段和候选链接推导下一步动作。
#[uniffi::export]
pub fn present_next_external_open_step(
    deep_link: Option<String>,
    web_url: Option<String>,
    stage: FfiExternalOpenStage,
) -> FfiExternalOpenStep {
    core::next_external_open_step(deep_link.as_deref(), web_url.as_deref(), stage.into()).into()
}
