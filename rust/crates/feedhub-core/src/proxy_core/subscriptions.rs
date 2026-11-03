//! 订阅 / 节点的增删改与合并（`ProxyCore.kt` 的"订阅 / 节点 / 导入"那一段的 Rust 版）。
//!
//! **请求不在这里**：取订阅要发 HTTP，走宿主（`HostProxyFetch`）。这一层只做"请求发出去之前 / 回来之后"
//! 的活：
//!
//! - 订阅的增删改（同 URL 重复加不会重复、名字为空时从 URL 里取一段）；
//! - 节点合并（同 id 的更新，**保留上次测速结果**；换某个订阅时先把那个订阅的旧节点删掉）；
//! - 哪个订阅该自动更新了（每条按自己的间隔，`0` = 关）；
//! - 取订阅失败时怎么记账（`last_error` 只放不含机密的文案）。
//!
//! 宿主只负责真的发和真的落盘。

use crate::proxy_core::model::{
    node_id_of, sha256_hex, ImportReport, ProxyCoreState, ProxyNode, SkippedEntry, Subscription,
    SubscriptionRoute, DEFAULT_SUBSCRIPTION_UA,
};

/// 一小时的毫秒数（`auto_update_hours` 换算用）。
const HOUR_MS: i64 = 3_600_000;

/// 加一个订阅（同 URL 重复加不会重复）。`auto_update_hours` 是这条订阅自己的自动更新间隔（0 = 关）。
///
/// 返回加进去（或者已经存在的）那条订阅。名字为空时从 URL 里取：`https://host/path` → `host` 前 40 个字符。
pub fn add_subscription(
    state: &ProxyCoreState,
    name: &str,
    url: &str,
    auto_update_hours: i32,
    now_ms: i64,
) -> Subscription {
    let clean = crate::hosted::kotlin_trim(url).to_string();
    let trimmed_name = crate::hosted::kotlin_trim(name);
    let sub = Subscription {
        id: crate::proxy_core::model::subscription_id_of(&clean),
        name: if trimmed_name.is_empty() {
            default_subscription_name(&clean)
        } else {
            trimmed_name.to_string()
        },
        url: clean,
        added_at: now_ms,
        last_update_at: None,
        last_error: None,
        node_count: 0,
        user_info: None,
        auto_update_hours,
    };
    if state.subscriptions.iter().any(|s| s.id == sub.id) {
        // Kotlin 的 `store.update { if (any) st else + sub }` 之后 `return sub`：返回的是**传进来的那份**，
        // 不是表里已经有的。照抄（两份内容一样，差异只在 `name` / `autoUpdateHours` 上）。
        return sub;
    }
    sub
}

/// 名字为空时从 URL 取一段：`sub.example.com`（`substringAfter("//").substringBefore('/')` 前 40 个字符）。
pub fn default_subscription_name(url: &str) -> String {
    let after = url.split_once("//").map(|(_, r)| r).unwrap_or(url);
    let host = after.split('/').next().unwrap_or(after);
    host.chars().take(40).collect()
}

/// 加完这条订阅之后的新状态（`store.update` 的等价物）。
pub fn with_subscription_added(state: &ProxyCoreState, sub: &Subscription) -> ProxyCoreState {
    let mut next = state.clone();
    if !next.subscriptions.iter().any(|s| s.id == sub.id) {
        next.subscriptions.push(sub.clone());
    }
    next
}

/// 改某条订阅的自动更新间隔（小时，0 = 关）。
pub fn set_subscription_auto_update(state: &ProxyCoreState, id: &str, hours: i32) -> ProxyCoreState {
    let mut next = state.clone();
    for sub in next.subscriptions.iter_mut() {
        if sub.id == id {
            sub.auto_update_hours = hours;
        }
    }
    next
}

pub fn rename_subscription(state: &ProxyCoreState, id: &str, name: &str) -> ProxyCoreState {
    let mut next = state.clone();
    for sub in next.subscriptions.iter_mut() {
        if sub.id == id {
            sub.name = name.to_string();
        }
    }
    next
}

/// 删订阅：它的节点一起删，选中的节点如果没了就清掉。
pub fn remove_subscription(state: &ProxyCoreState, id: &str) -> ProxyCoreState {
    let mut next = state.clone();
    let nodes: Vec<ProxyNode> = next
        .nodes
        .iter()
        .filter(|n| n.subscription_id.as_deref() != Some(id))
        .cloned()
        .collect();
    next.selected_node_id = next
        .selected_node_id
        .filter(|sel| nodes.iter().any(|n| &n.id == sel));
    next.nodes = nodes;
    next.subscriptions.retain(|s| s.id != id);
    next
}

/// 删一个节点；选中的正好是它就清掉选中。
pub fn remove_node(state: &ProxyCoreState, id: &str) -> ProxyCoreState {
    let mut next = state.clone();
    next.nodes.retain(|n| n.id != id);
    next.selected_node_id = next.selected_node_id.filter(|sel| sel != id);
    next
}

/// 手动选节点；`None` = 自动（延迟最低，`urltest`）。
pub fn select_node(state: &ProxyCoreState, id: Option<&str>) -> ProxyCoreState {
    let mut next = state.clone();
    next.selected_node_id = id.map(String::from);
    next
}

/// 把订阅内容并进节点表：同 id 的更新（**保留上次测速结果**）并换成这次的内容，新的加进去。
///
/// `replace_subscription_id` 非空时，先把这个订阅名下的旧节点全删掉（"更新订阅"的语义：
/// 订阅里删掉的节点要从我们的表里消失）。`subscription_id` 为空的（手动导入）永远不动。
pub fn merge_nodes(
    state: &ProxyCoreState,
    incoming: &[ProxyNode],
    skipped: &[SkippedEntry],
    replace_subscription_id: Option<&str>,
) -> (ProxyCoreState, ImportReport) {
    let mut added = 0i32;
    let mut updated = 0i32;
    let mut map: Vec<ProxyNode> = state
        .nodes
        .iter()
        .filter(|n| match replace_subscription_id {
            Some(id) => n.subscription_id.as_deref() != Some(id),
            None => true,
        })
        .cloned()
        .collect();
    for node in incoming {
        match map.iter_mut().find(|n| n.id == node.id) {
            Some(old) => {
                updated += 1;
                let last_latency_ms = old.last_latency_ms;
                let last_latency_at = old.last_latency_at;
                // `node.copy(lastLatencyMs = old.lastLatencyMs, lastLatencyAt = old.lastLatencyAt)`
                let mut fresh = node.clone();
                fresh.last_latency_ms = last_latency_ms;
                fresh.last_latency_at = last_latency_at;
                *old = fresh;
            }
            None => {
                added += 1;
                map.push(node.clone());
            }
        }
    }
    let mut next = state.clone();
    next.selected_node_id = next
        .selected_node_id
        .filter(|sel| map.iter().any(|n| &n.id == sel));
    for sub in next.subscriptions.iter_mut() {
        if let Some(replace) = replace_subscription_id {
            if sub.id == replace {
                sub.node_count = map
                    .iter()
                    .filter(|n| n.subscription_id.as_deref() == Some(replace))
                    .count() as i32;
            }
        } else {
            sub.node_count = map
                .iter()
                .filter(|n| n.subscription_id.as_deref() == Some(sub.id.as_str()))
                .count() as i32;
        }
    }
    next.nodes = map;
    (
        next,
        ImportReport {
            added,
            updated,
            skipped: skipped.to_vec(),
        },
    )
}

/// 哪些订阅该自动更新了（每条按自己的间隔，`auto_update_hours = 0` 就是永远不更新）。
///
/// `last_update_at` 为空的（刚加进来没成功过）**立刻算到点**。
pub fn due_subscriptions(state: &ProxyCoreState, now_ms: i64) -> Vec<String> {
    state
        .subscriptions
        .iter()
        .filter(|sub| {
            let hours = sub.auto_update_hours;
            hours > 0
                && match sub.last_update_at {
                    Some(at) => now_ms - at >= hours as i64 * HOUR_MS,
                    None => true,
                }
        })
        .map(|s| s.id.clone())
        .collect()
}

/// 取订阅成功之后记账：`last_update_at` / `last_error` / `node_count` / `userInfo`。
pub fn mark_subscription_updated(
    state: &ProxyCoreState,
    id: &str,
    user_info: Option<crate::proxy_core::model::SubscriptionUserInfo>,
    now_ms: i64,
) -> ProxyCoreState {
    let mut next = state.clone();
    let count = next
        .nodes
        .iter()
        .filter(|n| n.subscription_id.as_deref() == Some(id))
        .count() as i32;
    for sub in next.subscriptions.iter_mut() {
        if sub.id == id {
            sub.last_update_at = Some(now_ms);
            sub.last_error = None;
            sub.node_count = count;
            if user_info.is_some() {
                sub.user_info = user_info.clone();
            }
        }
    }
    next
}

/// 取订阅失败之后记账：只记一句**不含机密**的话（`HTTP 403` / `连不上` / `超时` …）。
pub fn mark_subscription_error(state: &ProxyCoreState, id: &str, reason: &str) -> ProxyCoreState {
    let mut next = state.clone();
    for sub in next.subscriptions.iter_mut() {
        if sub.id == id {
            sub.last_error = Some(reason.to_string());
        }
    }
    next
}

// ---------------------------------------------------------------------------
// 取订阅的请求怎么拼（发由宿主做）
// ---------------------------------------------------------------------------

/// "取订阅"这一步要宿主做的事。宿主按这个拼请求、发、把结果交回来。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FetchDecision {
    /// 发这个请求。`route` 告诉宿主用哪个客户端：
    /// - `Direct`：显式直连（`Proxy.NO_PROXY`）；
    /// - `System`：跟随 JVM 的系统代理；
    /// - `Core`：SOCKS5 指到本机核心。
    Fetch {
        url: String,
        user_agent: String,
        route: SubscriptionRoute,
        /// 走 `route = Core` 时具体连哪个端口（宿主据此派生客户端）。
        core_port: i32,
    },
    /// 不发（`route = Core` 但核心没在跑）。`reason` 已经是不含机密的文案（`failSubscription` 的口径）。
    Refused { reason: String },
    /// 订阅不在列表里了。
    NotFound,
}

/// 决定"这次取订阅要发什么请求"。**一个请求怎么拼完全在 Rust 决定**（URL 原样、UA 从设置里来、
/// 路线决定客户端），宿主只负责发。
///
/// `core_port`：`route = Core` 时核心在跑的端口（没在跑给 `None` → [`FetchDecision::Refused`]）。
pub fn plan_subscription_fetch(
    state: &ProxyCoreState,
    subscription_id: &str,
    core_port: Option<i32>,
) -> FetchDecision {
    let Some(sub) = state.subscriptions.iter().find(|s| s.id == subscription_id) else {
        return FetchDecision::NotFound;
    };
    let route = state.subscription_route;
    if route == SubscriptionRoute::Core && core_port.is_none() {
        return FetchDecision::Refused {
            reason: "选了「经核心」取订阅，但核心没在运行".to_string(),
        };
    }
    let ua = if crate::hosted::is_kotlin_blank(&state.subscription_ua) {
        DEFAULT_SUBSCRIPTION_UA.to_string()
    } else {
        state.subscription_ua.clone()
    };
    FetchDecision::Fetch {
        url: sub.url.clone(),
        user_agent: ua,
        route,
        core_port: core_port.unwrap_or(0),
    }
}

/// 取订阅的响应判定 + 解析（这一步在 Rust）。宿主把状态码 / body / `subscription-userinfo` 交回来。
pub struct FetchedSubscription {
    pub status: u16,
    pub body: String,
    pub user_info_header: Option<String>,
}

/// 取订阅的结果。
#[derive(Debug, Clone, PartialEq)]
pub enum SubscriptionUpdate {
    /// 成功：新状态 + 解析到几条。
    Ok {
        state: ProxyCoreState,
        parsed: usize,
    },
    /// 失败：新状态（记了 `last_error`）+ 给界面看的原因。
    Failed {
        state: ProxyCoreState,
        reason: String,
    },
}

/// 处理取订阅的响应：判状态码、解析、合并、记账。**和 Kotlin 的 `updateSubscription` 逐条对齐。**
pub fn finish_subscription_fetch(
    state: &ProxyCoreState,
    subscription_id: &str,
    fetched: &FetchedSubscription,
    now_ms: i64,
) -> SubscriptionUpdate {
    if !(200..300).contains(&fetched.status) {
        let reason = format!("HTTP {}", fetched.status);
        return SubscriptionUpdate::Failed {
            state: mark_subscription_error(state, subscription_id, &reason),
            reason,
        };
    }
    let info = crate::proxy_core::subscription::parse_subscription_user_info(
        fetched.user_info_header.as_deref(),
    );
    let parsed = crate::proxy_core::subscription::parse_node_source(
        &fetched.body,
        Some(subscription_id),
    );
    if parsed.nodes.is_empty() {
        let reason = if parsed.skipped.is_empty() {
            "没解析出节点".to_string()
        } else {
            format!(
                "没解析出节点（{} 条被跳过：{}）",
                parsed.skipped.len(),
                parsed.skipped[0].reason
            )
        };
        return SubscriptionUpdate::Failed {
            state: mark_subscription_error(state, subscription_id, &reason),
            reason,
        };
    }
    let count = parsed.nodes.len();
    let (mut next, _report) = merge_nodes(
        state,
        &parsed.nodes,
        &parsed.skipped,
        Some(subscription_id),
    );
    next = mark_subscription_updated(&next, subscription_id, info, now_ms);
    SubscriptionUpdate::Ok {
        state: next,
        parsed: count,
    }
}

/// 网络异常 → `last_error` 用的那句（**不含订阅地址 / 主机名**）。
pub fn subscription_network_error(kind: &str) -> String {
    crate::proxy_core::supervisor::network_reason(kind)
}

/// 手动导入（粘贴分享链接 / 一段 sing-box 出站 JSON）的完整流程：解析 + 合并。**不发请求。**
pub fn import_text(
    state: &ProxyCoreState,
    text: &str,
) -> (ProxyCoreState, ImportReport) {
    let parsed = crate::proxy_core::subscription::parse_node_source(text, None);
    merge_nodes(state, &parsed.nodes, &parsed.skipped, None)
}

/// 给导入用的节点 id（测试 / 门面用；生产的 id 在 [`crate::proxy_core::share_links::outbound_to_node`] 里算）。
pub fn node_id(kind: &str, outbound: &crate::proxy_core::json_text::V) -> String {
    node_id_of(kind, outbound)
}

/// 算订阅 id（宿主加订阅时也要用；两边算出来必须一样，否则"重复添加"会重复）。
pub fn subscription_id(url: &str) -> String {
    crate::proxy_core::model::subscription_id_of(url)
}

/// sha256 的小写十六进制（测试 / 门面用）。
pub fn sha256(text: &str) -> String {
    sha256_hex(text.as_bytes())
}

