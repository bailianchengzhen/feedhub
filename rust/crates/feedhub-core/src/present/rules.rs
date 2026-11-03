//! 小规则：空状态归因（`feed/FeedEmpty.kt`）、这条算不算视频（`video/VideoPosts.kt`）、
//! 关注按钮（`video/ImmersionFollow.kt`）、开哪个流（`video/MediaPlan.kt`）。
//!
//! 这些函数都只吃"事实"、吐"决定"，没有 IO、没有时钟。宿主（Kotlin）只把事实备齐、把决定画出来。

use crate::hosted::is_kotlin_blank;
use crate::player::manifest::{dash_manifest, DashTrack};

// ===========================================================================
// 「空白的时候该画什么」（FeedBlank）与空状态归因（FeedEmpty）
// ===========================================================================

/// 页面上空白的时候该画什么（Kotlin `FeedBlank`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeedBlank {
    /// 网格里有内容。
    Content,
    /// 还没有内容、也还不能说"空"：第一次加载还没走完 / 正在刷新。
    Loading,
    /// 加载过、也没在刷新，屏幕上还是空的。
    Empty,
}

/// `feedBlankFor`：有内容 → 内容；还没加载过 / 正在刷新 → 加载中；否则 → 空状态。
pub fn feed_blank_for(item_count: i32, refreshing: bool, loaded_once: bool) -> FeedBlank {
    if item_count > 0 {
        FeedBlank::Content
    } else if !loaded_once {
        FeedBlank::Loading
    } else if refreshing {
        FeedBlank::Loading
    } else {
        FeedBlank::Empty
    }
}

/// 空状态到底在说什么（Kotlin `FeedEmptyReason`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeedEmptyReason {
    /// 设置里一个平台都没点亮。
    NoPlatforms,
    /// 点亮的平台都没有这个流。
    NoPlatformHasFeed,
    /// 有这个流，但当前内容类型把它们筛掉了。
    ContentType,
    /// 请求到的平台都没登录。
    NotLoggedIn,
    /// 请求了、登录着，这一刷就是没有内容。
    NoContent,
    /// 断网（所有失败原因都是网络错误）。
    Offline,
    /// 有平台报错 / 超时。
    Failed,
}

/// 空状态给的出口（Kotlin `FeedEmptyAction`）。界面只把动作报出来，真正去哪儿由 App 接。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeedEmptyAction {
    GoLogin,
    GoRecommend,
    GoPlatforms,
    GoContentType,
}

/// 一个按钮：动作 + 文字（文字跟着原因走）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeedEmptyButton {
    pub action: FeedEmptyAction,
    pub label: String,
}

/// 空状态要画的东西。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeedEmptyCopy {
    pub reason: FeedEmptyReason,
    pub headline: String,
    pub body: String,
    pub buttons: Vec<FeedEmptyButton>,
    pub retry: bool,
}

/// 一个平台 + 它的显示名（顺序 = 插件顺序，文案里的名字顺序才稳定）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeedEmptyPlatform {
    pub id: String,
    pub label: String,
}

/// 这一刷失败的平台（`FeedState.errors` 的一项）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeedErrorFact {
    pub platform: String,
    /// 失败原因就是"网络错误"（标题行那条"没有网络"的同一个判据）。
    pub network_error: bool,
    /// 平台**明确回了"没登录"**（`NotLoggedInException`）。
    pub refused: bool,
}

/// 空状态判定要用的事实（Kotlin `FeedEmptyInputs`，只有 `AppGraph.feedEmptyInputs` 一处组装）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeedEmptyInputs {
    pub following: bool,
    /// 内容类型：`mixed` / `video` / `text`。
    pub content_type: ContentType,
    /// 设置里点亮、而且这台设备上能用的平台（插件顺序）。
    pub lit: Vec<FeedEmptyPlatform>,
    /// `lit` 里声明支持这个流的平台 id。
    pub supports_feed: Vec<String>,
    /// `supports_feed` 里被内容类型筛掉的。
    pub dropped_by_content_type: Vec<String>,
    /// 这一刷真的请求了的平台 id。
    pub requested: Vec<String>,
    /// 已经知道没登录 / 登录过期的平台 id。
    pub logged_out: Vec<String>,
    /// 这一刷平台明确回了"没登录"的平台 id。
    pub refused: Vec<String>,
    /// 这一刷失败的平台和原因。
    pub errors: Vec<FeedErrorFact>,
}

/// 内容类型（Kotlin `ContentType`：混合 / 只看视频 / 只看图文）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentType {
    Mixed,
    Video,
    Text,
}

/// 文案里的平台名：按 [order]（插件顺序）排，最多 `max` 家——再多就"…等 N 家"。
///
/// 从 [`FeedEmptyInputs::names_of`] 抽出来的独立入口：`feedEmptyFor` 内部用它，界面 / 测试也直接用它
/// （`FeedEmptyTest` 钉着"名字的顺序和"等 N 家"的写法）。
pub fn names_of(order: &[FeedEmptyPlatform], picked: &[String], max: usize) -> String {
    let mut all: Vec<String> = order
        .iter()
        .filter(|p| picked.iter().any(|id| id == &p.id))
        .map(|p| p.label.clone())
        .collect();
    // 不在 order 里的平台正常不会有（调用方都是从 lit 里筛的）；真出现就用它的 id，免得漏掉名字
    for id in picked {
        if !order.iter().any(|p| &p.id == id) {
            all.push(id.clone());
        }
    }
    if all.is_empty() {
        return String::new();
    }
    if all.len() <= max {
        all.join("、")
    } else {
        format!("{} 等 {} 家", all[..max].join("、"), all.len())
    }
}

impl FeedEmptyInputs {
    /// 顺序固定（插件顺序）地取平台名（实现在 [`names_of`]）。
    fn names_of(&self, picked: &[String], max: usize) -> String {
        names_of(&self.lit, picked, max)
    }
}

/// 空状态怎么画（`feedEmptyFor`，纯函数）。
///
/// 原则：**别把"这家没有这个流"说成"去登录"**，也别在没有意义的时候给「重试」；
/// 文案短、说人话，每种情况给一个真能做的动作。
pub fn feed_empty_for(inputs: &FeedEmptyInputs) -> FeedEmptyCopy {
    // 出错：`feedNoticeFor` 的判定（没有失败 = None；networkOnly = 全都是网络错误）
    let notice = if inputs.errors.is_empty() {
        None
    } else {
        Some((
            inputs.errors.len(),
            inputs.errors.iter().all(|e| e.network_error),
        ))
    };
    // 平台明确回"没登录"的不算"加载失败"
    let refused_errors: Vec<String> = inputs
        .errors
        .iter()
        .filter(|e| e.refused)
        .map(|e| e.platform.clone())
        .collect();
    if let Some(_notice) = notice {
        if !refused_errors.is_empty() && refused_errors.len() == inputs.errors.len() {
            return FeedEmptyCopy {
                reason: FeedEmptyReason::NotLoggedIn,
                headline: "要先登录".to_string(),
                body: format!(
                    "{}还没登录，登录完再点「重试」。",
                    inputs.names_of(&refused_errors, 3)
                ),
                buttons: vec![FeedEmptyButton {
                    action: FeedEmptyAction::GoLogin,
                    label: "去登录".to_string(),
                }],
                retry: true,
            };
        }
    }
    if let Some((_, true)) = notice {
        return FeedEmptyCopy {
            reason: FeedEmptyReason::Offline,
            headline: "没有网络".to_string(),
            body: "连上网络后点「重试」再试一次。".to_string(),
            buttons: vec![FeedEmptyButton {
                action: FeedEmptyAction::GoLogin,
                label: "去登录".to_string(),
            }],
            retry: true,
        };
    }
    if let Some((failures, false)) = notice {
        return FeedEmptyCopy {
            reason: FeedEmptyReason::Failed,
            headline: "内容没加载出来".to_string(),
            body: format!("{failures} 个平台加载失败，点「重试」再看看。"),
            buttons: vec![FeedEmptyButton {
                action: FeedEmptyAction::GoLogin,
                label: "去登录".to_string(),
            }],
            retry: true,
        };
    }

    // 一个平台都没请求：说清是哪一种"没有"，别让人去登录 / 重试
    if inputs.requested.is_empty() {
        if inputs.lit.is_empty() {
            return FeedEmptyCopy {
                reason: FeedEmptyReason::NoPlatforms,
                headline: "还没有点亮任何平台".to_string(),
                body: "到设置里点亮几家，首页和关注才有内容。".to_string(),
                buttons: vec![FeedEmptyButton {
                    action: FeedEmptyAction::GoPlatforms,
                    label: "去点亮平台".to_string(),
                }],
                retry: false,
            };
        }
        if inputs.supports_feed.is_empty() {
            let following = inputs.following;
            let all_lit: Vec<String> = inputs.lit.iter().map(|p| p.id.clone()).collect();
            let names = inputs.names_of(&all_lit, 3);
            let flow = if following { "关注流" } else { "推荐流" };
            let buttons = if following {
                vec![
                    FeedEmptyButton {
                        action: FeedEmptyAction::GoRecommend,
                        label: "去推荐看看".to_string(),
                    },
                    FeedEmptyButton {
                        action: FeedEmptyAction::GoPlatforms,
                        label: "去点亮平台".to_string(),
                    },
                ]
            } else {
                vec![FeedEmptyButton {
                    action: FeedEmptyAction::GoPlatforms,
                    label: "去点亮平台".to_string(),
                }]
            };
            return FeedEmptyCopy {
                reason: FeedEmptyReason::NoPlatformHasFeed,
                headline: if following { "关注里没有内容" } else { "首页没有内容" }.to_string(),
                body: format!("{names}没有{flow}，登录了也没有。"),
                buttons,
                retry: false,
            };
        }
        let dropped: Vec<String> = inputs
            .lit
            .iter()
            .filter(|p| inputs.dropped_by_content_type.iter().any(|id| id == &p.id))
            .map(|p| p.id.clone())
            .collect();
        let video = inputs.content_type == ContentType::Video;
        let what = if video { "只看视频" } else { "只看图文" };
        return FeedEmptyCopy {
            reason: FeedEmptyReason::ContentType,
            headline: if video {
                "只看视频时没有内容"
            } else {
                "只看图文时没有内容"
            }
            .to_string(),
            body: format!(
                "{what}时不推{}。改成「混合」再看看。",
                inputs.names_of(&dropped, 3)
            ),
            buttons: vec![FeedEmptyButton {
                action: FeedEmptyAction::GoContentType,
                label: "去改内容类型".to_string(),
            }],
            retry: false,
        };
    }

    // 请求了、也没出错：都没登录 → 去登录；否则就是这一刷真没内容
    let all_logged_out = inputs.requested.iter().all(|id| {
        inputs.logged_out.iter().any(|x| x == id) || inputs.refused.iter().any(|x| x == id)
    });
    if all_logged_out {
        return FeedEmptyCopy {
            reason: FeedEmptyReason::NotLoggedIn,
            headline: "要先登录".to_string(),
            body: format!(
                "{}还没登录，登录完再点「重试」。",
                inputs.names_of(&inputs.requested, 3)
            ),
            buttons: vec![FeedEmptyButton {
                action: FeedEmptyAction::GoLogin,
                label: "去登录".to_string(),
            }],
            retry: true,
        };
    }
    FeedEmptyCopy {
        reason: FeedEmptyReason::NoContent,
        headline: "这次没有内容".to_string(),
        body: "平台这次没给新内容，过会儿再点「重试」看看。".to_string(),
        buttons: Vec::new(),
        retry: true,
    }
}

// ===========================================================================
// 「这条内容算不算视频」
// ===========================================================================

/// 媒体第一项是不是视频（Kotlin `Media.Video`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaFirstKind {
    Image,
    Video,
}

/// `VideoPosts.isVideoPost`：kind 是视频，或者媒体里**第一项**是视频（贴吧的视频帖在信息流里 kind 是 POST）。
pub fn is_video_post(kind_is_video: bool, first_media: Option<MediaFirstKind>) -> bool {
    kind_is_video || first_media == Some(MediaFirstKind::Video)
}

// ===========================================================================
// 沉浸界面的「关注」按钮
// ===========================================================================

/// `immersionFollowVisible`：平台实现了 `FollowSource` **且**详情给了关注状态才显示。
pub fn immersion_follow_visible(has_follow_state: bool, has_follow_source: bool) -> bool {
    has_follow_state && has_follow_source
}

/// 关注按钮上的几个字（`ImmersionFollow.label`）。`following` 是 `Option<bool>`（接口没给状态 → `None`）。
pub fn follow_label(busy: bool, following: Option<bool>, forum: bool) -> String {
    if busy {
        "处理中…".to_string()
    } else if following == Some(true) {
        if forum { "已关注吧" } else { "已关注" }.to_string()
    } else if forum {
        "关注吧".to_string()
    } else {
        "关注".to_string()
    }
}

/// 关注 / 取关成功后的提示（`"已关注 " + target.name`）。
pub fn follow_success_message(follow: bool, target_name: &str) -> String {
    format!(
        "{}{}",
        if follow { "已关注 " } else { "已取消关注 " },
        target_name
    )
}

/// 没登录时的提示（`"请先在“我的”里登录" + platform.label`）。
pub fn follow_login_message(platform_label: &str) -> String {
    format!("请先在“我的”里登录{platform_label}")
}

/// 失败时的提示（`"操作失败：" + e.message`）。
///
/// `None` 是 Kotlin 的 `"${e.message}"` 在 `message == null` 时的字面行为（打印 `"null"`）——
/// 照抄，不"顺手修成空串"。
pub fn follow_failed_message(detail: Option<&str>) -> String {
    format!("操作失败：{}", detail.unwrap_or("null"))
}

// ===========================================================================
// 「这次开哪个流、用什么形式开」
// ===========================================================================

/// 一次播放用的流的形态（Kotlin `MediaPlan.Kind`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaPlanKind {
    /// 视频轨 + 音频轨，各自是带 sidx 的单文件分片。
    Dash,
    /// 两条独立的轨但没有 SegmentBase 信息。
    Merged,
    /// 音画一体的 HLS 播放列表。
    Hls,
    /// 音画一体的整段文件。
    Progressive,
}

/// DASH 一条轨的索引信息（Kotlin `DashTrack` / `SegmentBase`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DashTrackFacts {
    pub initialization: String,
    pub index_range: String,
    pub codecs: Option<String>,
    pub bandwidth: Option<i64>,
    pub width: Option<i32>,
    pub height: Option<i32>,
    pub frame_rate: Option<String>,
}

/// `MediaSource` 里"决定开哪个流"的那些事实。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaSourceFacts {
    pub video_urls: Vec<String>,
    pub audio_urls: Vec<String>,
    pub duration_ms: Option<i64>,
    pub dash_video: Option<DashTrackFacts>,
    pub dash_audio: Option<DashTrackFacts>,
}

/// 决定的结果：形态 + 排序后的地址 + （DASH 时）自己拼的 MPD。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaPlanOutcome {
    pub kind: MediaPlanKind,
    pub mpd: String,
    pub video_urls: Vec<String>,
    pub audio_urls: Vec<String>,
}

/// `mediaPlanOf`：把一次播放的流翻译成"用哪种形式开"；没有可播的地址返回 `None`。
///
/// 规则：
/// - 有 `dash` 信息（视频轨的 SegmentBase 齐全）→ DASH：拼 MPD，音轨没给 SegmentBase 时退回 MERGED；
/// - 没有 SegmentBase 但给了两条轨 → MERGED；
/// - 一条轨、地址是 `.m3u8` → HLS；
/// - 其余 → PROGRESSIVE。
pub fn media_plan_of(source: &MediaSourceFacts) -> Option<MediaPlanOutcome> {
    let video: Vec<String> = source
        .video_urls
        .iter()
        .filter(|u| !is_kotlin_blank(u))
        .cloned()
        .collect();
    if video.is_empty() {
        return None;
    }
    let audio: Vec<String> = source
        .audio_urls
        .iter()
        .filter(|u| !is_kotlin_blank(u))
        .cloned()
        .collect();
    let base = MediaPlanOutcome {
        kind: MediaPlanKind::Progressive,
        mpd: String::new(),
        video_urls: video,
        audio_urls: audio,
    };
    let dash = source.dash_video.as_ref().map(to_dash_track);
    let dash_audio = source.dash_audio.as_ref().map(to_dash_track);
    if let Some(track) = dash.as_ref() {
        if base.audio_urls.is_empty() || dash_audio.is_some() {
            let audio_url = if dash_audio.is_some() {
                base.audio_urls.first().map(|s| s.as_str())
            } else {
                None
            };
            let mpd = dash_manifest(
                &base.video_urls[0],
                audio_url,
                track,
                dash_audio.as_ref(),
                source.duration_ms,
            );
            return Some(MediaPlanOutcome {
                kind: MediaPlanKind::Dash,
                mpd,
                ..base
            });
        }
    }
    if !base.audio_urls.is_empty() {
        return Some(MediaPlanOutcome {
            kind: MediaPlanKind::Merged,
            ..base
        });
    }
    if is_hls_url(&base.video_urls[0]) {
        return Some(MediaPlanOutcome {
            kind: MediaPlanKind::Hls,
            ..base
        });
    }
    Some(base)
}

fn to_dash_track(t: &DashTrackFacts) -> DashTrack {
    DashTrack {
        initialization: t.initialization.clone(),
        index_range: t.index_range.clone(),
        codecs: t.codecs.clone(),
        bandwidth: t.bandwidth,
        width: t.width,
        height: t.height,
        frame_rate: t.frame_rate.clone(),
    }
}

/// 地址是不是一份 HLS 播放列表（按扩展名认，带查询串 / 片段也算）。
pub fn is_hls_url(url: &str) -> bool {
    let path = url.split('?').next().unwrap_or("");
    let path = path.split('#').next().unwrap_or(path);
    let b = path.as_bytes();
    b.len() >= 5 && b[b.len() - 5..].eq_ignore_ascii_case(b".m3u8")
}
