//! 验证码流程。
//!
//! 包含：
//! - [`policy`]：导航白名单策略（对齐 `CaptchaNavigationPolicy.kt`）；
//! - [`protocol`]：父子进程二进制协议编解码与拼帧（对齐 `HelperProtocol.kt`）；
//! - [`handler`]：验证码排队与校验规则、状态机（对齐 `CaptchaHandlerBase.kt`）；
//! - [`process`]：子进程命令行与尺寸换算纯函数（对齐 `CaptchaHelperProcess.kt`）。

pub mod handler;
pub mod policy;
pub mod process;
pub mod protocol;

pub use handler::{
    check_submit, current_url, format_token, generate_random_token, matches_active_page,
    parse_http_url, prompt_message, submit_url, valid_proof, ActivePage, HttpUrl, SubmitResult,
    DEFAULT_TIMEOUT_MS, PROMPT_STATUS_TIMEOUT, PROMPT_STATUS_WAITING, PROOF_PATH,
};
pub use policy::{
    is_main_frame_allowed, is_sub_frame_allowed, matches_domain, parse_url, port_of, ParsedUrl,
    SUB_FRAME_DOMAINS,
};
pub use process::{
    build_launcher_command, build_launcher_env, calculate_logical_size, redact_url, ADD_EXPORTS,
    FLAG, HELPER_MAIN,
};
pub use protocol::{
    decode_java_utf, encode_java_utf, plan_rect, read_command, read_event_header, truncate_utf16,
    write_bye, write_command, write_cursor, write_event_header, write_failed, write_frame_header,
    write_hello, write_loaded, write_rect, write_status, CodecError, FrameAssembler, FrameRectPlan,
    HelperCommand, HelperEvent, CMD_CLOSE, CMD_FOCUS, CMD_KEY, CMD_MOUSE, CMD_OPEN, CMD_RESIZE,
    CMD_WHEEL, EVT_BYE, EVT_CURSOR, EVT_FAILED, EVT_FRAME, EVT_HELLO, EVT_LOADED, EVT_STATUS,
    MAX_RECTS, MAX_SIDE, MAX_TEXT_LEN, VERSION,
};
