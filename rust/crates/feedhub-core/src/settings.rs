//! **设置的读 / 迁移 / 归一化 / 默认值**（Kotlin `app/Settings.kt` 的 `decodeSettings` + `defaultSettings` 那半边）。
//!
//! **界面状态和数据类留在 Kotlin**（`AppSettings` 是 Compose 的 `mutableStateOf`，设置页直接读它的字段），
//! 落盘的**时机**（防抖 / 关窗口 flush）也留在 Kotlin——那些是协程定时器，
//!
//! ## 为什么是"JSON 进、JSON 出"
//!
//! `AppSettings` 有三十多个字段、里面还有两层 sealed 接口（`ProxyChoice` / `ProxyConfig.Mode`），
//! 让 Rust 完整复刻 kotlinx 的序列化形状既没必要也容易漂（一个 `@EncodeDefault` 忘了就改坏存档）。
//! 所以边界是：
//!
//! - **Kotlin 负责编码**（`FeedJson.encodeToString(AppSettings.serializer(), …)`——那是数据类的机械序列化，
//!   格式的**唯一**真相）和解码（`FeedJson.decodeFromJsonElement`，缺字段自动用默认值）；
//! - **Rust 负责在这两步之间做全部策略**：读进来的那一棵树经过 [`normalize`]，缺的字段补上、
//!   老字段迁移掉、超范围的值收回、插件的默认值补进平台集合、境外平台的红线兜住。
//!
//! 也就是说**存盘的字节仍然是 kotlinx 写出来的那一份**（格式一个字节没变），而"该是什么值"只有
//! Rust 一份。`normalize` 对同一棵树跑两次结果一样（幂等），单测钉住这一点。
//!
//! ## 和 klib 的对照
//!
//! | 这里 | Kotlin |
//! |---|---|
//! | [`normalize`] | `decodeSettings` |
//! | [`defaults`] | `defaultSettings(platforms)` |
//! | [`PluginSpec`] | `PlatformPlugin` 的 `platform` / `defaults` / `overseas` |
//! | [`danmaku::normalize`] | `DanmakuSettings.normalized` |
//! | [`danmaku::from_legacy_density`] | `DanmakuSettings.fromLegacyDensity` |
//! | [`rules::normalize_rules`] | `ContentFilter.normalizeRules` |
//! | [`SAVE_DEBOUNCE_MS`] | `SETTINGS_SAVE_DEBOUNCE_MS` |

use crate::xhs::jsonord::{self, J};

/// 设置改动多久没有新的改动才写回存储（毫秒），和 Kotlin `SETTINGS_SAVE_DEBOUNCE_MS` 一样。
pub const SAVE_DEBOUNCE_MS: i64 = 500;

/// 老版本首页列数滑轨的默认值；老存档里等于它的当"自动"（用户没主动拖过）。
pub const LEGACY_DEFAULT_HOME_COLUMNS: i64 = 4;
/// 固定首页列数的范围。
pub const MIN_HOME_COLUMNS: i64 = 2;
pub const MAX_HOME_COLUMNS: i64 = 6;
/// 「每平台一批最多」的默认值。
pub const DEFAULT_MAX_PER_PLATFORM: i64 = 15;

/// 内容过滤一条名单最多几条 / 一条最多几个字（和 Kotlin `ContentFilter` 一样）。
pub const MAX_RULES: usize = 200;
pub const MAX_RULE_LENGTH: usize = 40;

/// 弹幕屏蔽词最多存多少条、一条最长多少字（和 Kotlin `DanmakuSettings` 一样）。
pub const DANMAKU_MAX_RULES: usize = 300;
pub const DANMAKU_MAX_RULE_LENGTH: usize = 200;

/// 老版本（`Platform` 还是 enum 时）的 11 个平台 id，**顺序就是老的 enum 顺序**。
///
/// 老存档里没有 `knownPlatforms` 时按这 11 个算"见过"（`PlatformCatalog.freshSince(null)` 的口径）。
pub const LEGACY_PLATFORMS: [&str; 11] = [
    "bili", "zhihu", "tieba", "heybox", "coolapk", "xhs", "douyin", "x", "yt", "reddit", "tiktok",
];

/// 一个平台插件里设置用得上的那几项（Kotlin `PlatformPlugin` 的 `platform` / `defaults` / `overseas`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginSpec {
    /// 平台 id（`bili` / `yt` / …）。
    pub id: String,
    /// `defaults.feedOn`：首页 / 关注默认开。
    pub feed_on: bool,
    /// `defaults.proxied`：「走代理的平台」默认点亮。
    pub proxied: bool,
    /// `defaults.autoTranslate`：「自动翻译」默认点亮。
    pub auto_translate: bool,
    /// `overseas`：境外平台（**不能直连**的红线靠它）。
    pub overseas: bool,
}

// ===========================================================================
// 入口
// ===========================================================================

/// 一份**老存档 / 手改过的存档** → 归一化之后的 JSON 文本。
///
/// 规则（逐条照 Kotlin `decodeSettings`）：
/// 1. `danmakuDensity`（老版只有一个"弹幕密度"）且没有 `danmaku` → 按
///    [`danmaku::from_legacy_density`] 换算；
/// 2. 有 `danmaku` → [`danmaku::normalize`]（超范围的值收回范围、屏蔽词收干净）；
/// 3. 没有 `homeColumnsMode` → 老存档：`homeColumns` 缺或等于 4 时 AUTO，否则 FIXED；
/// 4. `homeColumns` 夹到 2..6；`maxPerPlatform` 负数按 0；
/// 5. 三份名单按 [`rules::normalize_rules`] 收干净；
/// 6. 首页 / 关注：字段缺了用插件默认（`feedOn`）；**新装的插件**（不在 `knownPlatforms` 里）按默认补进来；
/// 7. `proxiedPlatforms`：字段缺了用插件默认（`proxied`）+ 新装的补进来 + **境外平台一律在里面**（红线）；
/// 8. `platformProxy`：**境外平台的「直连」一律不认**（红线），按「跟随全局」读（= 删掉这一项）；
/// 9. `translationPlatforms`：字段缺了用插件默认（`autoTranslate`）+ 新装的补进来；
/// 10. `knownPlatforms` = 存档里那份（缺了按老的 11 个）∪ 现在装着的所有插件（**总是写出去**）。
/// 11. `translationAndroidBackend`：
///     缺了 / `null` = **自动降级**（不写出去，让 Kotlin 的数据类默认值 `null` 生效）；
///     认的值只有 `"NPU"` / `"GPU"` / `"NPU_GPU"` / `"CPU_ONLY"`（大小写不敏感）；
///     **老版本那两个值 `"CPU"` / `"OPENCL"` 一律读成 `null` = 自动**——那时候这字段是必填的、
///     默认就是 `CPU`，那个 `CPU` 是产品默认值不是用户选择，当成"用户选了 CPU"就把所有人钉死在 CPU 上。
///     别的值（手改坏的）也读成自动。
///
/// 读不出来（不是 JSON 对象）就是 `None`，调用方退回"全套默认"。
pub fn normalize(text: &str, plugins: &[PluginSpec]) -> Option<String> {
    let root = jsonord::parse(text).ok()?;
    let fields = match root {
        J::Obj(fields) => fields,
        _ => return None,
    };
    let mut out = Settings::new(fields);
    apply(&mut out, plugins, KnownFallback::LegacyEleven);
    Some(out.render())
}

/// 全套默认值（Kotlin `defaultSettings(platforms)`）的 JSON 文本。
///
/// 只写"必须写出去"的字段（`homeColumnsMode` / `knownPlatforms` 是 `@EncodeDefault`）和几组平台集合；
/// 其余字段让 Kotlin 的解码器用数据类的默认值（那是 `AppSettings` 的真相，不在这里抄第二份）。
pub fn defaults(plugins: &[PluginSpec]) -> String {
    let mut out = Settings::new(Vec::new());
    // **默认值这条路**：`knownPlatforms` = 装着的全部（Kotlin `defaultSettings` 就是
    // `knownPlatforms = platforms.platforms.toSet()`），**不是**老的 11 个——那是"读老存档"的兜底。
    apply(&mut out, plugins, KnownFallback::Installed);
    out.render()
}

// ===========================================================================
// 一棵"字段有序"的设置树（就为了保住键的顺序：`jsonord` 的 J::Obj 是 Vec）
// ===========================================================================

struct Settings {
    fields: Vec<(String, J)>,
}

impl Settings {
    fn new(fields: Vec<(String, J)>) -> Self {
        Self { fields }
    }

    fn get(&self, key: &str) -> Option<&J> {
        self.fields.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }

    fn has(&self, key: &str) -> bool {
        self.get(key).is_some()
    }

    /// 有就换值、没有就追加到最后（`LinkedHashMap` 的语义：**已有键留在原位**）。
    fn set(&mut self, key: &str, value: J) {
        match self.fields.iter_mut().find(|(k, _)| k == key) {
            Some(slot) => slot.1 = value,
            None => self.fields.push((key.to_string(), value)),
        }
    }

    fn remove(&mut self, key: &str) {
        self.fields.retain(|(k, _)| k != key);
    }

    fn render(&self) -> String {
        let mut out = String::with_capacity(512);
        render_obj(&mut out, &self.fields);
        out
    }
}

fn render_obj(out: &mut String, fields: &[(String, J)]) {
    out.push('{');
    for (i, (key, value)) in fields.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        jsonord::quote(out, key);
        out.push(':');
        render_value(out, value);
    }
    out.push('}');
}

fn render_value(out: &mut String, value: &J) {
    match value {
        J::Null => out.push_str("null"),
        J::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        J::Num(raw) => out.push_str(raw),
        J::Str(s) => jsonord::quote(out, s),
        J::Arr(list) => {
            out.push('[');
            for (i, v) in list.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                render_value(out, v);
            }
            out.push(']');
        }
        J::Obj(fields) => render_obj(out, fields),
    }
}

// ===========================================================================
// 十条规则
// ===========================================================================

/// `knownPlatforms` 字段缺席时按什么算。
#[derive(Clone, Copy)]
enum KnownFallback {
    /// 读存档：老的 11 个平台（插件化之前没有这个字段）。
    LegacyEleven,
    /// 全套默认值：装着的全部（Kotlin `defaultSettings` 的口径）。
    Installed,
}

fn apply(out: &mut Settings, plugins: &[PluginSpec], fallback: KnownFallback) {
    // ---- 1 / 2：弹幕 ----
    if !out.has("danmaku") {
        if let Some(density) = out.get("danmakuDensity").and_then(|v| v.as_i64()) {
            out.set("danmaku", danmaku::from_legacy_density(density));
        }
    }
    if let Some(J::Obj(fields)) = out.get("danmaku").cloned() {
        out.set("danmaku", danmaku::normalize(&fields));
    }

    // ---- 3 / 4：首页列数 / 每平台封顶 ----
    let has_mode = out.has("homeColumnsMode");
    let columns = out.get("homeColumns").and_then(|v| v.as_i64());
    if !has_mode {
        let legacy = match columns {
            Some(v) if v != LEGACY_DEFAULT_HOME_COLUMNS => Some("FIXED"),
            _ => Some("AUTO"),
        };
        out.set("homeColumnsMode", J::Str(legacy.unwrap().to_string()));
    }
    if let Some(v) = columns {
        out.set(
            "homeColumns",
            J::Num(clamp(v, MIN_HOME_COLUMNS, MAX_HOME_COLUMNS).to_string()),
        );
    }
    if let Some(v) = out.get("maxPerPlatform").and_then(|v| v.as_i64()) {
        out.set("maxPerPlatform", J::Num(v.max(0).to_string()));
    }

    // ---- 5：三份名单 ----
    for key in ["blockWords", "outingKeywords", "outingCommunities"] {
        if let Some(J::Arr(list)) = out.get(key).cloned() {
            out.set(key, J::Arr(rules::normalize_rule_values(&list, MAX_RULES, MAX_RULE_LENGTH)));
        }
    }

    // ---- 6..10：平台集合 ----
    let installed: Vec<&str> = plugins.iter().map(|p| p.id.as_str()).collect();
    // `knownPlatforms`：**字段缺席**时按老的 11 个平台算（插件化之前没有这个字段）；
    // 显式 `null` / 空数组就是"一个都没见过"（Kotlin `if ("knownPlatforms" in json) settings.knownPlatforms
    // else Platform.LEGACY`，而 `settings.knownPlatforms` 遇到 null 会走数据类的默认值 —— 这里给空表，
    // 差别只在"手写了一个 null"这种没人会写的东西上，且方向是**更保守**：不把老的 11 个当成见过）。
    let known: Vec<String> = match out.get("knownPlatforms") {
        Some(J::Arr(list)) => list
            .iter()
            .filter_map(|v| v.as_str().map(|s| crate::hosted::platform_from_stored(s)))
            .collect(),
        Some(_) => Vec::new(),
        None => fallback.ids(plugins),
    };
    let fresh: Vec<&PluginSpec> = plugins
        .iter()
        .filter(|p| !known.iter().any(|k| *k == p.id))
        .collect();
    let overseas: Vec<&str> = plugins
        .iter()
        .filter(|p| p.overseas)
        .map(|p| p.id.as_str())
        .collect();

    for (key, pick, add_overseas) in [
        ("homePlatforms", PluginDefault::FeedOn, false),
        ("followingPlatforms", PluginDefault::FeedOn, false),
        ("proxiedPlatforms", PluginDefault::Proxied, true),
        ("translationPlatforms", PluginDefault::AutoTranslate, false),
    ] {
        let present = match out.get(key) {
            Some(J::Arr(list)) => Some(platform_list(list)),
            _ => None,
        };
        let mut set: Vec<String> = present.clone().unwrap_or_else(|| {
            // 字段缺了：用插件默认（`defaults.*`）
            plugins
                .iter()
                .filter(|p| pick.of(p))
                .map(|p| p.id.clone())
                .collect()
        });
        // 新装的插件按各自默认补进来（老的 11 个不受影响）
        for plugin in &fresh {
            if pick.of(plugin) && !set.iter().any(|s| s == &plugin.id) {
                set.push(plugin.id.clone());
            }
        }
        // 境外平台必须在「走代理的平台」里（红线）
        if add_overseas {
            for id in &overseas {
                if !set.iter().any(|s| s == id) {
                    set.push(id.to_string());
                }
            }
        }
        out.set(key, J::Arr(set.into_iter().map(|id| J::Str(stored_platform(&id))).collect()));
    }

    // 按平台指定代理：境外平台的「直连」不认。
    // 键可能写成老的 enum 名（`"YOUTUBE"`），判"是不是境外平台"之前先归一成 id。
    if let Some(J::Obj(entries)) = out.get("platformProxy").cloned() {
        let kept: Vec<(String, J)> = entries
            .into_iter()
            .filter(|(platform, choice)| {
                let id = crate::hosted::platform_from_stored(platform);
                let is_overseas = overseas.contains(&id.as_str());
                !(is_overseas && is_direct_choice(choice))
            })
            .collect();
        out.set("platformProxy", J::Obj(kept));
    }

    // 11. 安卓翻译的强制后端：认的值原样留着，老值 / 认不出来的删掉（= 自动）。
    //
    // **为什么删掉而不是写 `null`**：`FeedJson` 是 `encodeDefaults = false` + `explicitNulls = false`，
    // 序列化时 `null` 的字段根本不写出去；这里也照那个口径——把键拿掉就等于"用 Kotlin 的默认值
    // `null`（自动）"。写一个字面 `null` 也能读，但存档里多一行没意义的键，而且和 Kotlin 自己写的
    // 字节对不上（那个"老文件能读 / 新文件也读得回来"的门会红）。
    if let Some(value) = out.get("translationAndroidBackend").cloned() {
        // 认的值：原样留着（**不改大小写**——用户写什么就是什么，读的时候不敏感）；
        // 老值 / 认不出来的 / 不是字符串的：删掉 = 自动。
        if forced_android_backend(value.as_str()).is_none() {
            out.remove("translationAndroidBackend");
        }
    }

    // knownPlatforms 总是写出去（= 老的 11 个 ∪ 现在装着的全部）
    let mut merged: Vec<String> = known;
    for id in &installed {
        if !merged.iter().any(|k| k == id) {
            merged.push(id.to_string());
        }
    }
    out.set(
        "knownPlatforms",
        J::Arr(merged.into_iter().map(|id| J::Str(stored_platform(&id))).collect()),
    );
}

/// 平台集合字段的默认值从插件描述的哪一项来。
#[derive(Clone, Copy)]
enum PluginDefault {
    FeedOn,
    Proxied,
    AutoTranslate,
}

impl KnownFallback {
    fn ids(self, plugins: &[PluginSpec]) -> Vec<String> {
        match self {
            KnownFallback::LegacyEleven => LEGACY_PLATFORMS.iter().map(|s| s.to_string()).collect(),
            KnownFallback::Installed => plugins.iter().map(|p| p.id.clone()).collect(),
        }
    }
}

impl PluginDefault {
    fn of(self, p: &PluginSpec) -> bool {
        match self {
            PluginDefault::FeedOn => p.feed_on,
            PluginDefault::Proxied => p.proxied,
            PluginDefault::AutoTranslate => p.auto_translate,
        }
    }
}

/// 一组平台：**保留存档里的顺序**（Kotlin 是 `Set<Platform>` = `LinkedHashSet`），丢掉重复和非字符串，
/// 老的 enum 名（`"BILIBILI"`）换成 id（`"bili"`）。
fn platform_list(list: &[J]) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(list.len());
    for value in list {
        let Some(text) = value.as_str() else { continue };
        let id = crate::hosted::platform_from_stored(text);
        if out.iter().any(|s| *s == id) {
            continue;
        }
        out.push(id);
    }
    out
}

/// 写进存档时用的平台写法：老的 11 家写 enum 名（旧版本读得回），新的写 id。
fn stored_platform(id: &str) -> String {
    crate::hosted::platform_to_stored(id)
}

/// 安卓翻译的**强制后端**认不认这个值（`None` = 删掉这一项 = 自动）。
///
/// 口径：只认
/// `NPU` / `GPU` / `NPU_GPU` / `CPU_ONLY`（大小写不敏感）。
/// 那个 `CPU` 是产品默认值不是用户选择，当成用户选择就把所有人钉死在 CPU 上。
fn forced_android_backend(value: Option<&str>) -> Option<String> {
    let v = value?.trim();
    match v.to_ascii_lowercase().as_str() {
        "npu" | "gpu" | "npu_gpu" | "cpu_only" => Some(v.to_string()),
        _ => None,
    }
}

/// `ProxyChoice.Direct`（`{"mode":"direct"}`）。境外平台拿到它按「跟随全局」读（= 删掉这一项）。
fn is_direct_choice(value: &J) -> bool {
    matches!(value, J::Obj(fields) if fields.iter().any(|(k, v)| k == "mode" && v.as_str() == Some("direct")))
}

fn clamp(v: i64, lo: i64, hi: i64) -> i64 {
    v.max(lo).min(hi)
}

// ===========================================================================
// 弹幕设置（Kotlin `DanmakuSettings`）
// ===========================================================================

pub mod danmaku {
    use super::*;

    /// 显示区域的八档（1/8、1/4 …… 全屏）。
    pub const AREA_OPTIONS: [f64; 8] = [0.125, 0.25, 0.375, 0.5, 0.625, 0.75, 0.875, 1.0];

    /// **把超出范围的值收回范围里**（手改了存档、以后改了范围时用）；屏蔽词去掉空白、重复和过长的。
    ///
    /// 逐条照 Kotlin `DanmakuSettings.normalized`。**不改**的字段（布尔 / 枚举）原样写回，
    /// 不认识的字段也原样留着（Kotlin 那边 `ignoreUnknownKeys` 会忽略它们）。
    pub fn normalize(fields: &[(String, J)]) -> J {
        let mut out = fields.to_vec();
        set_num(&mut out, "opacity", clamp_f64(num(fields, "opacity", 0.85), 0.3, 1.0, 0.85));
        set_num(&mut out, "fontScale", clamp_f64(num(fields, "fontScale", 1.0), 0.5, 2.0, 1.0));
        // **这四个是 Kotlin 的 `Int`**（`fontWeight` / `weightLevel` / `mergeWindowMs` / `mergeThreshold`）：
        // 必须写 `4` 而不是 `4.0`，否则 kotlinx 解出来会报
        // "Failed to parse literal '4.0' as an int value"、`SettingsStore` 整份退回默认值。
        set_int(&mut out, "fontWeight", clamp_f64(num(fields, "fontWeight", 4.0), 1.0, 9.0, 4.0));
        set_num(&mut out, "strokeWidth", clamp_f64(num(fields, "strokeWidth", 1.5), 0.0, 4.0, 1.5));
        set_num(&mut out, "area", nearest_area(num(fields, "area", 0.5)));
        set_num(&mut out, "lineHeight", clamp_f64(num(fields, "lineHeight", 1.4), 0.8, 2.2, 1.4));
        set_num(&mut out, "scrollSeconds", clamp_f64(num(fields, "scrollSeconds", 8.0), 2.0, 15.0, 8.0));
        set_num(&mut out, "fixedSeconds", clamp_f64(num(fields, "fixedSeconds", 4.0), 2.0, 15.0, 4.0));
        set_int(&mut out, "weightLevel", clamp_f64(num(fields, "weightLevel", 0.0), 0.0, 10.0, 0.0));
        set_int(&mut out, "mergeWindowMs", clamp_f64(num(fields, "mergeWindowMs", 500.0), 100.0, 3000.0, 500.0));
        set_int(&mut out, "mergeThreshold", clamp_f64(num(fields, "mergeThreshold", 2.0), 2.0, 10.0, 2.0));
        if let Some(J::Arr(list)) = out.iter().find(|(k, _)| k == "blockRules").map(|(_, v)| v.clone()) {
            let mut seen: Vec<String> = Vec::new();
            for value in list {
                let Some(text) = value.as_str() else { continue };
                let word = text.trim();
                if word.is_empty() || word.chars().count() > DANMAKU_MAX_RULE_LENGTH {
                    continue;
                }
                if seen.iter().any(|s| s == word) {
                    continue;
                }
                seen.push(word.to_string());
                if seen.len() >= DANMAKU_MAX_RULES {
                    break;
                }
            }
            set_value(
                &mut out,
                "blockRules",
                J::Arr(seen.into_iter().map(J::Str).collect()),
            );
        }
        J::Obj(out)
    }

    /// 旧版只有一个"弹幕密度"（0~100）：换算成显示区域（取最近一档）+ 智能云屏蔽等级。
    ///
    /// 逐条照 Kotlin `DanmakuSettings.fromLegacyDensity`：
    /// `area = nearestArea(0.25 + 0.55 × d / 100)`、
    /// `weightLevel = round((100 - d) / 100 × 6)`（Kotlin 的 `roundToInt` = 四舍五入、.5 往上）。
    pub fn from_legacy_density(density: i64) -> J {
        let d = clamp(density, 0, 100) as f64;
        let area = nearest_area(0.25 + 0.55 * d / 100.0);
        let level = ((100.0 - d) / 100.0 * 6.0).round();
        let mut fields: Vec<(String, J)> = Vec::new();
        // `area` 是 Kotlin 的 `Float`（八档小数值），`weightLevel` 是 `Int`——两者的数字形状不能混。
        set_num(&mut fields, "area", area);
        set_int(&mut fields, "weightLevel", level);
        // 其余选项用 Kotlin 数据类的默认值：这里**不写**它们（Kotlin 解码时自动补），
        // 所以换算出来的对象只带这两项 —— 和 Kotlin `DanmakuSettings(area=…, weightLevel=…)` 等价。
        J::Obj(fields)
    }

    /// 离 `value` 最近的一档显示区域（Kotlin `AREA_OPTIONS.minBy { abs(it - value) }`）。
    ///
    /// 距离相同时**取靠前的那一档**（Kotlin 的 `minBy` 是稳定取第一个）。非有限值 → 0.5。
    pub fn nearest_area(value: f64) -> f64 {
        if !value.is_finite() {
            return 0.5;
        }
        let mut best = AREA_OPTIONS[0];
        let mut best_distance = (best - value).abs();
        for option in AREA_OPTIONS.iter().skip(1) {
            let distance = (option - value).abs();
            if distance < best_distance {
                best = *option;
                best_distance = distance;
            }
        }
        best
    }

    fn num(fields: &[(String, J)], key: &str, fallback: f64) -> f64 {
        fields
            .iter()
            .find(|(k, _)| k == key)
            .and_then(|(_, v)| v.as_f64())
            .unwrap_or(fallback)
    }

    /// `Float.clampedTo(range, fallback)`：非有限值用 fallback，其余夹到范围里。
    fn clamp_f64(value: f64, lo: f64, hi: f64, fallback: f64) -> f64 {
        if !value.is_finite() {
            return fallback;
        }
        value.max(lo).min(hi)
    }

    fn set_num(fields: &mut Vec<(String, J)>, key: &str, value: f64) {
        set_value(fields, key, J::Num(number_text(value)));
    }

    /// 给 Kotlin 的 **`Int`** 字段写一个整数（不带 `.0`）。
    ///
    /// kotlinx 的 `JsonPrimitive.int` 只认 `4` 这种字面量，`4.0` 会抛
    /// `Failed to parse literal '4.0' as an int value`——而 `SettingsStore` 读设置时是
    /// `runCatching { decodeSettings(...) }.getOrNull()`，一抛就**整份退回默认值**，用户改过的设置全丢。
    fn set_int(fields: &mut Vec<(String, J)>, key: &str, value: f64) {
        // `clamp_f64` 给进来的一定是有限值且已经夹过范围，`round()` 就是 Kotlin `roundToInt()` 的形状。
        let raw = value.round();
        let text = if raw == 0.0 { "0".to_string() } else { format!("{raw:.0}") };
        set_value(fields, key, J::Num(text));
    }

    fn set_value(fields: &mut Vec<(String, J)>, key: &str, value: J) {
        match fields.iter_mut().find(|(k, _)| k == key) {
            Some(slot) => slot.1 = value,
            None => fields.push((key.to_string(), value)),
        }
    }

    /// 一个 `f64` 写成 JSON 数字：整数也写 `.0`（kotlinx 的 `Float.toString()` 形状）。
    pub fn number_text(value: f64) -> String {
        if value.is_nan() {
            return "NaN".to_string();
        }
        if value.is_infinite() {
            return if value > 0.0 { "Infinity" } else { "-Infinity" }.to_string();
        }
        let text = format!("{value:?}");
        if text.contains('.') || text.contains('e') || text.contains('E') {
            text
        } else {
            format!("{text}.0")
        }
    }
}

// ===========================================================================
// 内容过滤名单（Kotlin `ContentFilter.normalizeRules`）
// ===========================================================================

pub mod rules {
    use super::*;

    /// 读存档 / 设置页加一条时把名单收拾干净：去空白、丢掉空的和过长的、按 [`normalize`] 出来的样子去重、
    /// 最多 `max` 条。**保留用户写的原文**（不把 `Ａ` 改成 `A`），只是比较时忽略这些差别。
    pub fn normalize_rule_values(list: &[J], max: usize, max_length: usize) -> Vec<J> {
        let mut seen: Vec<String> = Vec::new();
        let mut out: Vec<J> = Vec::new();
        for value in list {
            let Some(text) = value.as_str() else { continue };
            let word = text.trim();
            if word.is_empty() || word.chars().count() > max_length {
                continue;
            }
            let key = normalize(word);
            if seen.iter().any(|s| *s == key) {
                continue;
            }
            seen.push(key);
            out.push(J::Str(word.to_string()));
            if out.len() >= max {
                break;
            }
        }
        out
    }

    /// 把一条关键词 / 社区名归一成用来比较的样子：去首尾空白、全角转半角、小写、连续空白压成一个。
    ///
    /// **和 `crate::brain::filter::normalize` 是同一套口径**（那边是给匹配用的，这里是给去重用的），
    /// 所以直接调它，不抄第二份。
    pub fn normalize(text: &str) -> String {
        crate::brain::filter::normalize(text)
    }
}

// ===========================================================================
// 设置驱动的运行时规则
// ===========================================================================

/// 流类型（用于按内容过滤请求平台）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeedKind {
    Recommend,
    Following,
}

/// 内容类型（用于按内容过滤请求平台）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentType {
    Mixed,
    Video,
    Text,
}

/// 平台请求能力描述。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlatformRequestSpec {
    pub id: String,
    pub playable_video: bool,
    pub video_only_feeds: Vec<FeedKind>,
}

/// 这次要请求哪些平台：按 enabled 顺序筛已安装插件。
/// - MIXED: 全留；
/// - VIDEO: 看 playable_video；
/// - TEXT: 看 kind !in video_only_feeds。
pub fn requested_platforms(
    kind: FeedKind,
    content_type: ContentType,
    enabled: &[String],
    plugins: &[PlatformRequestSpec],
) -> Vec<String> {
    let mut out = Vec::new();
    for id in enabled {
        let plugin = match plugins.iter().find(|p| p.id == *id) {
            Some(p) => p,
            None => continue,
        };
        let keep = match content_type {
            ContentType::Mixed => true,
            ContentType::Video => plugin.playable_video,
            ContentType::Text => !plugin.video_only_feeds.contains(&kind),
        };
        if keep && !out.iter().any(|s| s == id) {
            out.push(id.clone());
        }
    }
    out
}

/// 这个平台单独选的那一档代理：
/// - 存了 Custom 用 Custom；
/// - 境外平台（overseas == true）永远返回 Follow，绝不返回 Direct（风控红线！）；
/// - 其余情况：若存了 Direct 返回 Direct；若在 proxied_platforms 里返回 Follow；否则 Direct。
pub fn proxy_choice_for(
    stored: Option<&crate::proxy::ProxyChoice>,
    is_proxied: bool,
    overseas: bool,
) -> crate::proxy::ProxyChoice {
    match stored {
        Some(crate::proxy::ProxyChoice::Custom { host, port, kind }) => {
            crate::proxy::ProxyChoice::Custom {
                host: host.clone(),
                port: *port,
                kind: *kind,
            }
        }
        _ => {
            if overseas {
                crate::proxy::ProxyChoice::Follow
            } else if stored == Some(&crate::proxy::ProxyChoice::Direct) {
                crate::proxy::ProxyChoice::Direct
            } else if is_proxied {
                crate::proxy::ProxyChoice::Follow
            } else {
                crate::proxy::ProxyChoice::Direct
            }
        }
    }
}

/// 改一个平台的代理档，同步计算新的 (platform_proxy, proxied_platforms)：
/// - 境外平台选 Direct 强制退回 Follow（红线）；
/// - Custom 存入 map 并点亮集合；
/// - Follow 删 map 项并点亮集合；
/// - Direct 删 map 项并点灭集合。
pub fn with_proxy_choice(
    platform: &str,
    choice: &crate::proxy::ProxyChoice,
    overseas: bool,
    current_platform_proxy: &[(String, crate::proxy::ProxyChoice)],
    current_proxied_platforms: &[String],
) -> (Vec<(String, crate::proxy::ProxyChoice)>, Vec<String>) {
    let safe = if overseas && matches!(choice, crate::proxy::ProxyChoice::Direct) {
        &crate::proxy::ProxyChoice::Follow
    } else {
        choice
    };
    let mut next_map: Vec<(String, crate::proxy::ProxyChoice)> = current_platform_proxy.to_vec();
    let mut next_proxied: Vec<String> = current_proxied_platforms.to_vec();
    let lit = match safe {
        crate::proxy::ProxyChoice::Custom { .. } => {
            if let Some(slot) = next_map.iter_mut().find(|(p, _)| p == platform) {
                slot.1 = safe.clone();
            } else {
                next_map.push((platform.to_string(), safe.clone()));
            }
            true
        }
        crate::proxy::ProxyChoice::Follow => {
            next_map.retain(|(p, _)| p != platform);
            true
        }
        crate::proxy::ProxyChoice::Direct => {
            next_map.retain(|(p, _)| p != platform);
            false
        }
    };
    if lit {
        if !next_proxied.iter().any(|p| p == platform) {
            next_proxied.push(platform.to_string());
        }
    } else {
        next_proxied.retain(|p| p != platform);
    }
    (next_map, next_proxied)
}

/// 这个平台这次该走什么代理（一步到位入口，内部直接调用 crate::proxy::resolve）。
pub fn resolve_proxy_for(
    stored: Option<&crate::proxy::ProxyChoice>,
    is_proxied: bool,
    overseas: bool,
    global: &crate::proxy::ProxyMode,
    exit_lock: bool,
) -> crate::proxy::ProxyMode {
    let choice = proxy_choice_for(stored, is_proxied, overseas);
    crate::proxy::resolve(&choice, global, exit_lock, overseas)
}

/// 因为出口锁关着、代理用不了而改成直连时的通知文案（一步到位入口，内部直接调用 crate::proxy::fallback_reason）。
pub fn direct_fallback_reason(
    stored: Option<&crate::proxy::ProxyChoice>,
    is_proxied: bool,
    overseas: bool,
    global: &crate::proxy::ProxyMode,
    exit_lock: bool,
) -> Option<String> {
    let choice = proxy_choice_for(stored, is_proxied, overseas);
    crate::proxy::fallback_reason(&choice, global, exit_lock)
}

/// 出口锁关闭回退直连的通知去重跟踪器。
///
/// 规则：
/// - reason == null 时，只有 choice == Follow (is_follow == true) 才清空 lastFallbackReason；
/// - reason != null && reason != lastFallbackReason 时更新并返回 Some(reason) 以触发一次通知；
/// - 其余情况返回 None。
#[derive(Debug, Default)]
pub struct DirectFallbackTracker {
    last_reason: std::sync::Mutex<Option<String>>,
}

impl DirectFallbackTracker {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn track(&self, is_follow: bool, reason: Option<&str>) -> Option<String> {
        let mut guard = self.last_reason.lock().unwrap();
        match reason {
            None => {
                if is_follow {
                    *guard = None;
                }
                None
            }
            Some(r) => {
                if guard.as_deref() != Some(r) {
                    *guard = Some(r.to_string());
                    Some(r.to_string())
                } else {
                    None
                }
            }
        }
    }

    /// 一次调用完成单平台代理判定与出口锁回退通知去重（避免 `proxyProvider` 热路径连跨 4 次 FFI）。
    pub fn resolve_and_track(
        &self,
        stored: Option<&crate::proxy::ProxyChoice>,
        is_proxied: bool,
        overseas: bool,
        global: &crate::proxy::ProxyMode,
        exit_lock: bool,
    ) -> (crate::proxy::ProxyMode, Option<String>) {
        let choice = proxy_choice_for(stored, is_proxied, overseas);
        let is_follow = matches!(choice, crate::proxy::ProxyChoice::Follow);
        let reason = crate::proxy::fallback_reason(&choice, global, exit_lock);
        let notify = self.track(is_follow, reason.as_deref());
        let mode = crate::proxy::resolve(&choice, global, exit_lock, overseas);
        (mode, notify)
    }
}

