//! 续页游标 `ResumeCursor`（对外不透明）：`fh1:<skip>:<base64url>`。

const PREFIX: &str = "fh1:";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedCursor {
    pub platform: Option<String>,
    pub skip: i32,
}

pub fn decode(raw: Option<&str>) -> DecodedCursor {
    let raw = match raw {
        Some(r) => r,
        None => return DecodedCursor { platform: None, skip: 0 },
    };
    if !raw.starts_with(PREFIX) {
        return DecodedCursor {
            platform: Some(raw.to_string()),
            skip: 0,
        };
    }
    let body = &raw[PREFIX.len()..];
    let colon = match body.find(':') {
        Some(idx) if idx > 0 => idx,
        _ => return DecodedCursor {
            platform: Some(raw.to_string()),
            skip: 0,
        },
    };
    let skip: i32 = match body[..colon].parse::<i32>() {
        Ok(s) if s >= 0 => s,
        _ => return DecodedCursor {
            platform: Some(raw.to_string()),
            skip: 0,
        },
    };
    let encoded = &body[colon + 1..];
    if encoded.is_empty() {
        return DecodedCursor { platform: None, skip };
    }
    let bytes = match crate::sync::b64::decode(encoded) {
        Some(b) => b,
        None => return DecodedCursor {
            platform: Some(raw.to_string()),
            skip: 0,
        },
    };
    let cursor = match String::from_utf8(bytes) {
        Ok(s) => s,
        Err(_) => return DecodedCursor {
            platform: Some(raw.to_string()),
            skip: 0,
        },
    };
    DecodedCursor {
        platform: Some(cursor),
        skip,
    }
}

pub fn encode(platform: Option<&str>, skip: i32) -> String {
    let safe_skip = skip.max(0);
    let encoded = match platform.filter(|s| !s.is_empty()) {
        Some(s) => crate::sync::b64::encode(s.as_bytes()),
        None => String::new(),
    };
    format!("{PREFIX}{safe_skip}:{encoded}")
}

