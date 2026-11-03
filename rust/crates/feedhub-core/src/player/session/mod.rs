//! **播放状态机**（`app/.../video/PlaybackSession.kt` 里"不是界面也不是引擎"的那一半）。
//!
//! Kotlin 的那个类有三层：Compose 状态（界面读）、引擎调用（GStreamer / Media3）、夹在中间的
//! **决策**。第三层在这里：
//!
//! | 这条规则 | 原来在哪 | 为什么是决策 |
//! |---|---|---|
//! | "播完时点播放 = 从头重播" | `togglePlay` | 实测 #1：Media3 在 `STATE_ENDED` 上会不会自己回到开头不由我们控制，而进度 / 时间是引擎位置的镜像，结果画面回开头了、进度条还停在结尾 |
//! | 续播判定（离结尾 5 秒内就从 0 开始） | `loadPart` | 用户上次看到 89/90 秒，重开不该只剩 1 秒 |
//! | 切后台 / 回前台**自动接着播** | `pauseForBackground` / `resumeFromBackground` | 用户自己点过暂停就作废；被音频焦点 / 拔耳机压下来的不算"在播" |
//! | 长按加速 + 锁定 | `longPress*` | 一次长按只翻一次锁；锁着时按住是"临时恢复原速" |
//! | 倍速的"临时 / 锁定 / 用户自己选"三种状态 | `holdSpeed` / `setSpeed` / `releaseSpeed` | 优先顺序容易写错，而且和弹幕的滚动时长耦合 |
//! | 取流失败怎么说话 / 要不要给"登录"按钮 / 要不要提"换节点" | `loadPart` 的 catch | 风控红线（不重试、不优化），而且要按平台是不是境外决定提示 |
//! | 进度该不该存（存哪一 P、播到结尾就归零） | `saveProgress` | 和"哪一 P"耦合 |
//!
//! **不在这里**：Compose 的 `mutableStateOf`（界面）、`engine.open/seekTo/setRate`（引擎）、
//! `withContext(io) { source.play() }`（协程 + 收发）。
//!
//! 这一层是**纯状态机**：每个方法接一个"现在"（`now_ms` 之类由宿主给），返回宿主要做的事
//! （[`Effect`]），不读时钟、不调引擎。

use crate::play::PlayInfo;

/// 倍速的上 / 下界（和 `PlayerEngine.setRate` 的约定一样：0.25~4）。
pub const MIN_RATE: f32 = 0.25;
pub const MAX_RATE: f32 = 4.0;

/// 长按加速的倍速（`PlayerGestureMath.LONG_PRESS_SPEED`）。
pub const LONG_PRESS_SPEED: f32 = 3.0;

/// 离结尾不到这么多毫秒就当成"看完了"，重开从头播。
pub const RESUME_TAIL_MS: i64 = 5000;

/// 进度离结尾不到这么多毫秒就存成 0（下次从头播）。
pub const SAVE_TAIL_MS: i64 = 3000;

/// 宿主要做的一件事（引擎调用 / 界面状态变更 / 存盘）。
#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    /// `engine.play()`
    Play,
    /// `engine.pause()`
    Pause,
    /// `state.ended = false; state.jump(0); engine.seekTo(0); engine.play()`（播完再点播放）
    Replay,
    /// `engine.seekTo(ms)`
    Seek(i64),
    /// `engine.setRate(rate)`
    SetRate(f32),
    /// 存进度（`library.saveProgress(key, pos, duration)`）。
    SaveProgress {
        /// 存下去的位置（播到结尾是 0）。
        position_ms: i64,
        duration_ms: Option<i64>,
    },
    /// 记"上次看到第几 P"（`lastPartStore.put`；第一 P 存 `None`）。
    SavePart(Option<String>),
}

/// 播放器此刻的状态（宿主把 `engine.state` 的这些字段同步进来）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlaybackState {
    /// 想播放着（没暂停）；缓冲时也是 `true`。
    pub playing: bool,
    /// 正在缓冲 / 打开中。
    pub buffering: bool,
    pub position_ms: i64,
    pub duration_ms: i64,
    pub ended: bool,
    pub speed: f32,
    pub muted: bool,
    pub volume: i32,
}

impl Default for PlaybackState {
    fn default() -> Self {
        Self {
            playing: false,
            buffering: false,
            position_ms: 0,
            duration_ms: 0,
            ended: false,
            speed: 1.0,
            muted: false,
            volume: 100,
        }
    }
}

/// 一次播放（一个视频页 / 一个行内视频）的状态机。
#[derive(Debug, Clone, Default)]
pub struct Session {
    /// 切后台前是不是在播 → 回前台要接着播。用户自己点过播放 / 暂停就清掉。
    resume_on_foreground: bool,
    /// 临时倍速之前的倍速（长按按下的那一刻）。
    held_from: Option<f32>,
    /// 长按倍速锁着（松手也保持 3 倍速）。
    speed_locked: bool,
    /// 长按的手指还按着。
    speed_pressing: bool,
    /// 这次长按已经上滑过了（一次长按只翻一次锁）。
    lock_flipped: bool,
    /// 上次存下去的进度 / 分 P（避免重复写）。
    last_saved_ms: i64,
    last_saved_part: i32,
    /// 上次存的时候的分 P 下标（`-1` = 还没存过）。
    part_index: i32,
}

impl Session {
    pub fn new() -> Self {
        Self {
            last_saved_ms: -1,
            last_saved_part: -1,
            part_index: 0,
            ..Self::default()
        }
    }

    /// 现在在不在"临时加速"/"锁定"里。
    pub fn speed_locked(&self) -> bool {
        self.speed_locked
    }

    pub fn speed_pressing(&self) -> bool {
        self.speed_pressing
    }

    /// 底座倍速（提示里显示"恢复成几倍"用）：临时加速中是**加速前**那个，否则就是当前。
    pub fn base_speed(&self, current: f32) -> f32 {
        self.held_from.unwrap_or(current)
    }

    /// 播放 / 暂停。
    ///
    /// **播完（`ended`）时点播放 = 从头重播**：用 [`Effect::Replay`]，不直接 `play()`——
    /// 引擎在"播完"这个状态上会不会自己回到开头不由我们控制，而进度 / 时间是引擎位置的镜像，
    /// 两者不一致就是"画面回开头了、进度条还停在结尾"。
    pub fn toggle_play(&mut self, state: &PlaybackState) -> Effect {
        // 用户自己点过播放 / 暂停：回前台不再自动续播
        self.resume_on_foreground = false;
        if state.playing {
            Effect::Pause
        } else if state.ended {
            Effect::Replay
        } else {
            Effect::Play
        }
    }

    /// 切后台 / 锁屏：照旧立刻停，记下停之前是不是在播。
    ///
    /// `onPause` / `onStop` 各调一次，**第二次不会把记号抹掉**（`state.playing` 已经是 false 了，
    /// 所以"记记号"只在真的在播时发生；`resume_on_foreground` 本身不清）。
    pub fn pause_for_background(&mut self, state: &PlaybackState) -> Effect {
        if state.playing {
            self.resume_on_foreground = true;
        }
        Effect::Pause
    }

    /// 回前台：切走前在播才续播，从原位置接着放（播完的不重播）。返回 `None` = 什么都不做。
    pub fn resume_from_background(&mut self, state: &PlaybackState) -> Option<Effect> {
        if !self.resume_on_foreground {
            return None;
        }
        self.resume_on_foreground = false;
        if !state.playing && !state.ended {
            Some(Effect::Play)
        } else {
            // 已经在播 / 已经播完：不用动引擎（Kotlin 那边只 poke 一下界面）
            None
        }
    }

    /// 快进 / 快退 `delta_ms`（夹在 `0..=duration`）。
    pub fn seek_by(&self, state: &PlaybackState, delta_ms: i64) -> Effect {
        let upper = if state.duration_ms > 0 {
            state.duration_ms
        } else {
            i64::MAX
        };
        Effect::Seek((state.position_ms + delta_ms).clamp(0, upper))
    }

    /// 用户自己设置倍速：取消临时加速、解除锁定。
    pub fn set_speed(&mut self, rate: f32) -> Effect {
        self.held_from = None;
        self.speed_locked = false;
        Effect::SetRate(clamp_rate(rate))
    }

    /// 临时倍速：按下时调（比如长按 3 倍加速），记住当前倍速；已经在临时加速中就只换速率，
    /// 原值还是最早那个。
    pub fn hold_speed(&mut self, state: &PlaybackState, rate: f32) -> Effect {
        if self.held_from.is_none() {
            self.held_from = Some(state.speed);
        }
        Effect::SetRate(clamp_rate(rate))
    }

    /// 松手：恢复成临时加速之前的倍速；没有在临时加速中就什么都不做。
    pub fn release_speed(&mut self) -> Option<Effect> {
        let back = self.held_from?;
        self.held_from = None;
        Some(Effect::SetRate(clamp_rate(back)))
    }

    /// 长按按下：没锁 → 临时 3 倍速；锁着 → 临时恢复成锁定之前的倍速（松手回到 3 倍速）。
    pub fn long_press_start(&mut self, state: &PlaybackState) -> Effect {
        self.speed_pressing = true;
        self.lock_flipped = false;
        if self.speed_locked {
            if let Some(base) = self.held_from {
                return Effect::SetRate(clamp_rate(base));
            }
        }
        self.speed_locked = false;
        self.hold_speed(state, LONG_PRESS_SPEED)
    }

    /// 长按时上滑：没锁 → 锁定 3 倍速；锁着 → 取消锁定。一次长按只算一次。
    /// 返回切换后是不是锁着。
    pub fn long_press_swipe_up(&mut self) -> bool {
        if self.speed_pressing && !self.lock_flipped {
            self.lock_flipped = true;
            self.speed_locked = !self.speed_locked;
        }
        self.speed_locked
    }

    /// 松手（或手势被取消）：锁着 → 3 倍速；没锁 → 恢复长按之前的倍速。
    pub fn long_press_end(&mut self) -> Option<Effect> {
        if !self.speed_pressing {
            return None;
        }
        self.speed_pressing = false;
        if self.speed_locked {
            Some(Effect::SetRate(clamp_rate(LONG_PRESS_SPEED)))
        } else {
            self.release_speed()
        }
    }

    /// 快一档 / 慢一档（键盘 `]` / `[`）。
    pub fn step_speed(&mut self, state: &PlaybackState, delta: i32) -> Effect {
        let next = crate::player::select::PlaybackSpeed::step(state.speed, delta);
        self.set_speed(next)
    }

    /// 续播位置：`resume == true` 时从上次的进度接着看（离结尾不到 [`RESUME_TAIL_MS`] 就从头）；
    /// 否则就是 `start_ms`。
    pub fn start_position(resume: bool, saved_ms: Option<i64>, start_ms: i64, duration_ms: i64) -> i64 {
        let mut start = if resume {
            saved_ms.unwrap_or(0)
        } else {
            start_ms
        };
        if duration_ms > 0 && start >= duration_ms - RESUME_TAIL_MS {
            start = 0;
        }
        start
    }

    /// 该不该存进度；该存就返回要做的事。
    ///
    /// - 没播（位置和时长都是 0）不存；
    /// - 位置和分 P 都没变不存；
    /// - 播到结尾（离结尾不到 [`SAVE_TAIL_MS`]）存 **0**，下次从头播；
    /// - 第一 P 不记（`SavePart(None)` 只在**之前记过**的时候发，所以单 P 视频不会往存储里写东西）。
    pub fn save_progress(
        &mut self,
        part_index: i32,
        position_ms: i64,
        duration_ms: i64,
        part_id: Option<&str>,
    ) -> Vec<Effect> {
        let idx = part_index.max(0);
        if self.part_index != idx {
            self.part_index = idx;
        }
        if (position_ms == 0 && duration_ms == 0) || (position_ms == self.last_saved_ms && idx == self.last_saved_part) {
            return Vec::new();
        }
        self.last_saved_ms = position_ms;
        self.last_saved_part = idx;
        let saved_pos = if duration_ms > 0 && position_ms >= duration_ms - SAVE_TAIL_MS {
            0
        } else {
            position_ms
        };
        let mut out = vec![Effect::SaveProgress {
            position_ms: saved_pos,
            duration_ms: if duration_ms > 0 { Some(duration_ms) } else { None },
        }];
        // 第一 P 也要发一次 `SavePart(None)`：宿主那边"上次看到第几 P"可能是别的值，
        // 要它把这一格清掉（Kotlin 原版 `if (store.get(partKey) != want) store.put(partKey, want)`）。
        out.push(Effect::SavePart(part_id.filter(|_| idx > 0).map(|s| s.to_string())));
        out
    }

    /// 取流失败怎么说：把异常分成"登录能解决 / 风控 / 其它"三类，给出界面文案 +
    /// 要不要显示"登录"按钮 + 要不要补一句"换个代理节点"。
    ///
    /// 风控红线：**不重试、不换参数**，只说话。
    pub fn play_failure(&self, failure: Failure, platform_label: &str, proxied: bool) -> FailureText {
        match failure {
            Failure::NotLoggedIn => FailureText {
                needs_login: true,
                error: format!("登录{platform_label}后再播这个视频"),
                hint: None,
            },
            Failure::RiskControl => FailureText {
                needs_login: false,
                error: format!("被{platform_label}风控拦了，稍后再试"),
                // 走代理的平台（境外）才提"换节点"；国内平台说这个会让人以为要去改代理
                hint: if proxied {
                    Some("如果一直这样，换个代理节点再试".to_string())
                } else {
                    None
                },
            },
            Failure::Other(ref message) => FailureText {
                needs_login: false,
                error: format!("取不到播放地址：{message}"),
                hint: None,
            },
        }
    }

    /// 选不出流时的文案（Kotlin 里是 `error = "没有可播放的流"`）。
    pub fn no_stream_text(&self) -> String {
        "没有可播放的流".to_string()
    }

    /// 这个视频没有可播的分 P 时的文案。
    pub fn no_part_text(&self) -> String {
        "这个视频没有可播放的内容".to_string()
    }

    /// 首次打开该播第几 P：上次看到的那一 P（找不到就第一 P）。
    ///
    /// `parts` 是各分 P 的 id（顺序就是界面上看到的顺序），`saved` 是上次记下的那个 id。
    pub fn initial_part_index(parts: &[String], saved: Option<&str>) -> usize {
        match saved {
            None => 0,
            Some(id) => parts.iter().position(|p| p == id).unwrap_or(0),
        }
    }

    /// 时长：播放地址给的优先，没有就用分 P 自带的。
    pub fn duration_of(info: &PlayInfo, part_duration_sec: Option<i32>) -> i64 {
        match info.duration_ms {
            Some(ms) => ms,
            None => part_duration_sec.map(|s| s as i64 * 1000).unwrap_or(0),
        }
    }
}

/// 取流失败的三种（宿主把异常映射成这个）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    /// 登录能解决（平台要求登录 / YouTube 的"确认不是机器人"没登录时也归到这一类）。
    NotLoggedIn,
    /// 风控：**不重试**，让用户自己决定什么时候再来。
    RiskControl,
    /// 其它（拼一句 `取不到播放地址：…`）。
    Other(String),
}

/// [`Session::play_failure`] 的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailureText {
    pub needs_login: bool,
    pub error: String,
    pub hint: Option<String>,
}

/// 倍速夹到引擎认的范围（Kotlin `PlayerEngine.setRate` 的约定是 0.25~4）。
///
/// Kotlin 的 `PlaybackSession` 不夹（它相信调用方都是档位表里的值）；这里**也不夹**到档位，
/// 只夹到引擎能表达的范围，并且把 `NaN` 换成 1.0——一条 NaN 进了引擎会让 GStreamer 那边报得很难查。
pub fn clamp_rate(rate: f32) -> f32 {
    if !rate.is_finite() {
        return 1.0;
    }
    rate.clamp(MIN_RATE, MAX_RATE)
}

/// 一次播放的"引擎侧效果"清单（宿主按顺序做）。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Batch {
    pub effects: Vec<Effect>,
}

impl Batch {
    pub fn push(&mut self, effect: Effect) {
        self.effects.push(effect);
    }

    pub fn is_empty(&self) -> bool {
        self.effects.is_empty()
    }
}

