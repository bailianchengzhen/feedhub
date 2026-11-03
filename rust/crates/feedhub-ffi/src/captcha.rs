//! 验证码 UniFFI 绑定层。

use feedhub_core::captcha::{self, ActivePage, CodecError, HelperCommand, HelperEvent};

#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum FfiCaptchaError {
    #[error("{detail}")]
    Codec { detail: String },
    #[error("{detail}")]
    BadInput { detail: String },
    #[error("{detail}")]
    Io { detail: String },
}

impl From<CodecError> for FfiCaptchaError {
    fn from(e: CodecError) -> Self {
        Self::Codec {
            detail: e.to_string(),
        }
    }
}

#[derive(uniffi::Record)]
pub struct FfiCaptchaParsedUrl {
    pub scheme: String,
    pub host: String,
    pub port: i32,
    pub effective_port: i32,
}

#[derive(uniffi::Record, Clone)]
pub struct FfiCaptchaActivePage {
    pub scheme: String,
    pub host: String,
    pub port: i32,
    pub path: String,
    pub token: String,
}

#[derive(uniffi::Record)]
pub struct FfiCaptchaHttpUrl {
    pub scheme: String,
    pub host: String,
    pub port: i32,
    pub encoded_path: String,
    pub query: Option<String>,
}

#[derive(uniffi::Enum, PartialEq, Eq, Debug)]
pub enum FfiCaptchaSubmitResult {
    Accepted { proof: String },
    WrongToken,
    BadProof,
    NoPage,
}

#[derive(uniffi::Enum, PartialEq, Debug)]
pub enum FfiCaptchaCommand {
    Open {
        url: String,
        width: i32,
        height: i32,
        scale: f32,
    },
    Resize {
        width: i32,
        height: i32,
        scale: f32,
    },
    Mouse {
        id: i32,
        x: i32,
        y: i32,
        modifiers: i32,
        button: i32,
        click_count: i32,
        stamp: i64,
    },
    Wheel {
        x: i32,
        y: i32,
        modifiers: i32,
        rotation: i32,
        precise: f64,
        stamp: i64,
    },
    Key {
        id: i32,
        key_code: i32,
        key_char: u16,
        modifiers: i32,
        location: i32,
        stamp: i64,
    },
    Focus {
        focused: bool,
    },
    Close,
}

#[derive(uniffi::Enum, PartialEq, Debug)]
pub enum FfiCaptchaEvent {
    Hello {
        version: i32,
        pid: i64,
    },
    Status {
        text: String,
    },
    Frame {
        width: i32,
        height: i32,
        rects: i32,
        paint_stamp: i64,
        input_stamp: i64,
    },
    Cursor {
        cursor_type: i32,
    },
    Loaded {
        http_status: i32,
    },
    Failed {
        detail: String,
    },
    Bye,
}

#[derive(uniffi::Record)]
pub struct FfiCaptchaRectPlan {
    pub valid: bool,
    pub inside: bool,
    pub bytes_per_row: i32,
    pub rows: i32,
    pub row_stride_bytes: i32,
    pub initial_offset_bytes: i32,
    pub total_bytes: i64,
}

#[derive(uniffi::Record)]
pub struct FfiCaptchaEnvEntry {
    pub key: String,
    pub value: Option<String>,
}

#[derive(uniffi::Record)]
pub struct FfiCaptchaSize {
    pub width: i32,
    pub height: i32,
}

impl From<FfiCaptchaCommand> for HelperCommand {
    fn from(c: FfiCaptchaCommand) -> Self {
        match c {
            FfiCaptchaCommand::Open {
                url,
                width,
                height,
                scale,
            } => HelperCommand::Open {
                url,
                width,
                height,
                scale,
            },
            FfiCaptchaCommand::Resize {
                width,
                height,
                scale,
            } => HelperCommand::Resize {
                width,
                height,
                scale,
            },
            FfiCaptchaCommand::Mouse {
                id,
                x,
                y,
                modifiers,
                button,
                click_count,
                stamp,
            } => HelperCommand::Mouse {
                id,
                x,
                y,
                modifiers,
                button,
                click_count,
                stamp,
            },
            FfiCaptchaCommand::Wheel {
                x,
                y,
                modifiers,
                rotation,
                precise,
                stamp,
            } => HelperCommand::Wheel {
                x,
                y,
                modifiers,
                rotation,
                precise,
                stamp,
            },
            FfiCaptchaCommand::Key {
                id,
                key_code,
                key_char,
                modifiers,
                location,
                stamp,
            } => HelperCommand::Key {
                id,
                key_code,
                key_char,
                modifiers,
                location,
                stamp,
            },
            FfiCaptchaCommand::Focus { focused } => HelperCommand::Focus { focused },
            FfiCaptchaCommand::Close => HelperCommand::Close,
        }
    }
}

impl From<HelperCommand> for FfiCaptchaCommand {
    fn from(c: HelperCommand) -> Self {
        match c {
            HelperCommand::Open {
                url,
                width,
                height,
                scale,
            } => FfiCaptchaCommand::Open {
                url,
                width,
                height,
                scale,
            },
            HelperCommand::Resize {
                width,
                height,
                scale,
            } => FfiCaptchaCommand::Resize {
                width,
                height,
                scale,
            },
            HelperCommand::Mouse {
                id,
                x,
                y,
                modifiers,
                button,
                click_count,
                stamp,
            } => FfiCaptchaCommand::Mouse {
                id,
                x,
                y,
                modifiers,
                button,
                click_count,
                stamp,
            },
            HelperCommand::Wheel {
                x,
                y,
                modifiers,
                rotation,
                precise,
                stamp,
            } => FfiCaptchaCommand::Wheel {
                x,
                y,
                modifiers,
                rotation,
                precise,
                stamp,
            },
            HelperCommand::Key {
                id,
                key_code,
                key_char,
                modifiers,
                location,
                stamp,
            } => FfiCaptchaCommand::Key {
                id,
                key_code,
                key_char,
                modifiers,
                location,
                stamp,
            },
            HelperCommand::Focus { focused } => FfiCaptchaCommand::Focus { focused },
            HelperCommand::Close => FfiCaptchaCommand::Close,
        }
    }
}

impl From<FfiCaptchaEvent> for HelperEvent {
    fn from(e: FfiCaptchaEvent) -> Self {
        match e {
            FfiCaptchaEvent::Hello { version, pid } => HelperEvent::Hello { version, pid },
            FfiCaptchaEvent::Status { text } => HelperEvent::Status { text },
            FfiCaptchaEvent::Frame {
                width,
                height,
                rects,
                paint_stamp,
                input_stamp,
            } => HelperEvent::Frame {
                width,
                height,
                rects,
                paint_stamp,
                input_stamp,
            },
            FfiCaptchaEvent::Cursor { cursor_type } => HelperEvent::Cursor { cursor_type },
            FfiCaptchaEvent::Loaded { http_status } => HelperEvent::Loaded { http_status },
            FfiCaptchaEvent::Failed { detail } => HelperEvent::Failed { message: detail },
            FfiCaptchaEvent::Bye => HelperEvent::Bye,
        }
    }
}

impl From<HelperEvent> for FfiCaptchaEvent {
    fn from(e: HelperEvent) -> Self {
        match e {
            HelperEvent::Hello { version, pid } => FfiCaptchaEvent::Hello { version, pid },
            HelperEvent::Status { text } => FfiCaptchaEvent::Status { text },
            HelperEvent::Frame {
                width,
                height,
                rects,
                paint_stamp,
                input_stamp,
            } => FfiCaptchaEvent::Frame {
                width,
                height,
                rects,
                paint_stamp,
                input_stamp,
            },
            HelperEvent::Cursor { cursor_type } => FfiCaptchaEvent::Cursor { cursor_type },
            HelperEvent::Loaded { http_status } => FfiCaptchaEvent::Loaded { http_status },
            HelperEvent::Failed { message } => FfiCaptchaEvent::Failed { detail: message },
            HelperEvent::Bye => FfiCaptchaEvent::Bye,
        }
    }
}

impl From<captcha::SubmitResult> for FfiCaptchaSubmitResult {
    fn from(r: captcha::SubmitResult) -> Self {
        match r {
            captcha::SubmitResult::Accepted { proof } => FfiCaptchaSubmitResult::Accepted { proof },
            captcha::SubmitResult::WrongToken => FfiCaptchaSubmitResult::WrongToken,
            captcha::SubmitResult::BadProof => FfiCaptchaSubmitResult::BadProof,
            captcha::SubmitResult::NoPage => FfiCaptchaSubmitResult::NoPage,
        }
    }
}

impl From<FfiCaptchaActivePage> for ActivePage {
    fn from(p: FfiCaptchaActivePage) -> Self {
        ActivePage {
            scheme: p.scheme,
            host: p.host,
            port: p.port,
            path: p.path,
            token: p.token,
        }
    }
}

#[uniffi::export]
pub fn ffi_captcha_parse_url(raw: String) -> Option<FfiCaptchaParsedUrl> {
    captcha::parse_url(&raw).map(|p| FfiCaptchaParsedUrl {
        effective_port: p.effective_port(),
        scheme: p.scheme,
        host: p.host,
        port: p.port,
    })
}

#[uniffi::export]
pub fn ffi_captcha_port_of(url: String) -> i32 {
    captcha::port_of(&url)
}

#[uniffi::export]
pub fn ffi_captcha_is_main_frame_allowed(port: i32, url: String) -> bool {
    captcha::is_main_frame_allowed(port, &url)
}

#[uniffi::export]
pub fn ffi_captcha_is_sub_frame_allowed(url: String) -> bool {
    captcha::is_sub_frame_allowed(&url)
}

#[uniffi::export]
pub fn ffi_captcha_valid_proof(proof: String) -> bool {
    captcha::valid_proof(&proof)
}

#[uniffi::export]
pub fn ffi_captcha_generate_random_token() -> Result<String, FfiCaptchaError> {
    captcha::generate_random_token().map_err(|detail| FfiCaptchaError::BadInput { detail })
}

#[uniffi::export]
pub fn ffi_captcha_submit_url(token: String) -> String {
    captcha::submit_url(&token)
}

#[uniffi::export]
pub fn ffi_captcha_prompt_message(platform_label: String, challenge_message: String) -> String {
    captcha::prompt_message(&platform_label, &challenge_message)
}

#[uniffi::export]
pub fn ffi_captcha_parse_http_url(url: String) -> Option<FfiCaptchaHttpUrl> {
    captcha::parse_http_url(&url).map(|u| FfiCaptchaHttpUrl {
        scheme: u.scheme,
        host: u.host,
        port: u.port,
        encoded_path: u.encoded_path,
        query: u.query,
    })
}

#[uniffi::export]
pub fn ffi_captcha_current_url(page: FfiCaptchaActivePage) -> String {
    let p: ActivePage = page.into();
    captcha::current_url(&p)
}

#[uniffi::export]
pub fn ffi_captcha_matches_active_page(page: Option<FfiCaptchaActivePage>, url: String) -> bool {
    let core_page = page.map(ActivePage::from);
    captcha::matches_active_page(core_page.as_ref(), &url)
}

#[uniffi::export]
pub fn ffi_captcha_check_submit(
    page: Option<FfiCaptchaActivePage>,
    url: String,
) -> FfiCaptchaSubmitResult {
    let core_page = page.map(ActivePage::from);
    captcha::check_submit(core_page.as_ref(), &url).into()
}

#[uniffi::export]
pub fn ffi_captcha_encode_command(cmd: FfiCaptchaCommand) -> Result<Vec<u8>, FfiCaptchaError> {
    let mut out = Vec::new();
    let core_cmd: HelperCommand = cmd.into();
    captcha::write_command(&core_cmd, &mut out)?;
    Ok(out)
}

#[uniffi::export]
pub fn ffi_captcha_decode_command(
    bytes: Vec<u8>,
) -> Result<Option<FfiCaptchaCommand>, FfiCaptchaError> {
    match captcha::read_command(&bytes)? {
        Some((cmd, _)) => Ok(Some(cmd.into())),
        None => Ok(None),
    }
}

#[uniffi::export]
pub fn ffi_captcha_encode_event_header(
    event: FfiCaptchaEvent,
) -> Result<Vec<u8>, FfiCaptchaError> {
    let mut out = Vec::new();
    let core_event: HelperEvent = event.into();
    captcha::write_event_header(&core_event, &mut out)?;
    Ok(out)
}

#[uniffi::export]
pub fn ffi_captcha_decode_event_header(
    bytes: Vec<u8>,
) -> Result<Option<FfiCaptchaEvent>, FfiCaptchaError> {
    match captcha::read_event_header(&bytes)? {
        Some((ev, _)) => Ok(Some(ev.into())),
        None => Ok(None),
    }
}

#[uniffi::export]
pub fn ffi_captcha_plan_rect(
    frame_w: i32,
    frame_h: i32,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
) -> FfiCaptchaRectPlan {
    let p = captcha::plan_rect(frame_w, frame_h, x, y, w, h);
    FfiCaptchaRectPlan {
        valid: p.valid,
        inside: p.inside,
        bytes_per_row: p.bytes_per_row,
        rows: p.rows,
        row_stride_bytes: p.row_stride_bytes,
        initial_offset_bytes: p.initial_offset_bytes,
        total_bytes: p.total_bytes,
    }
}

#[uniffi::export]
pub fn ffi_captcha_build_launcher_command(
    java_bin: Option<String>,
    can_execute_java: bool,
    class_path: String,
    main_class: String,
    app_path: Option<String>,
    can_execute_app: bool,
) -> Result<Vec<String>, FfiCaptchaError> {
    captcha::build_launcher_command(
        java_bin.as_deref(),
        can_execute_java,
        &class_path,
        &main_class,
        app_path.as_deref(),
        can_execute_app,
    )
    .map_err(|detail| FfiCaptchaError::BadInput { detail })
}

#[uniffi::export]
pub fn ffi_captcha_build_launcher_env(
    app_path: Option<String>,
    archive_path: Option<String>,
) -> Vec<FfiCaptchaEnvEntry> {
    captcha::build_launcher_env(app_path.as_deref(), archive_path.as_deref())
        .into_iter()
        .map(|(key, value)| FfiCaptchaEnvEntry { key, value })
        .collect()
}

#[uniffi::export]
pub fn ffi_captcha_calculate_logical_size(
    width_px: i32,
    height_px: i32,
    density: f32,
) -> FfiCaptchaSize {
    let (w, h) = captcha::calculate_logical_size(width_px, height_px, density);
    FfiCaptchaSize {
        width: w,
        height: h,
    }
}

#[uniffi::export]
pub fn ffi_captcha_redact_url(url: Option<String>) -> String {
    captcha::redact_url(url.as_deref())
}

