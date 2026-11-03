//! 验证码子进程启动参数与尺寸换算纯计算（对齐 `CaptchaHelperProcess.kt`）。

use crate::html::kotlin_is_whitespace;

pub const FLAG: &str = "--captcha-helper";
pub const HELPER_MAIN: &str = "feedhub.app.captcha.helper.CaptchaHelperMain";

pub const ADD_EXPORTS: &[&str] = &[
    "--add-exports",
    "java.base/java.lang=ALL-UNNAMED",
    "--add-exports",
    "java.desktop/sun.awt=ALL-UNNAMED",
    "--add-exports",
    "java.desktop/sun.java2d=ALL-UNNAMED",
];

/// 拼装子进程命令行
pub fn build_launcher_command(
    java_bin: Option<&str>,
    can_execute_java: bool,
    class_path: &str,
    main_class: &str,
    app_path: Option<&str>,
    can_execute_app: bool,
) -> Result<Vec<String>, String> {
    if let Some(java) = java_bin {
        if can_execute_java {
            let mut cmd = Vec::with_capacity(7 + ADD_EXPORTS.len());
            cmd.push(java.to_string());
            for &exp in ADD_EXPORTS {
                cmd.push(exp.to_string());
            }
            cmd.push("-Dfile.encoding=UTF-8".to_string());
            cmd.push("-cp".to_string());
            cmd.push(class_path.to_string());
            cmd.push(main_class.to_string());
            return Ok(cmd);
        }
    }

    if let Some(launcher) = app_path {
        if main_class == HELPER_MAIN && can_execute_app {
            return Ok(vec![launcher.to_string(), FLAG.to_string()]);
        }
    }

    Err(format!(
        "起不了验证码子进程：{} 不能执行，也没有 jpackage 启动器",
        java_bin.unwrap_or("没有 java.home")
    ))
}

/// 拼装子进程环境变量字典（None 表示删除该变量）
pub fn build_launcher_env(
    app_path: Option<&str>,
    archive_path: Option<&str>,
) -> Vec<(String, Option<String>)> {
    let mut opts = vec![
        "-Djava.awt.headless=true".to_string(),
        "-XX:+UseSerialGC".to_string(),
        "-Xmx256m".to_string(),
    ];

    if app_path.is_some() {
        if let Some(archive) = archive_path {
            if !archive.chars().any(kotlin_is_whitespace) {
                opts.push(format!("-XX:SharedArchiveFile={archive}"));
                opts.push("-XX:+AutoCreateSharedArchive".to_string());
                opts.push("-Xlog:cds=off".to_string());
                opts.push("-Xlog:cds+dynamic=off".to_string());
            }
        }
    }

    vec![
        ("JAVA_TOOL_OPTIONS".to_string(), Some(opts.join(" "))),
        ("JDK_JAVA_OPTIONS".to_string(), None),
        ("WAYLAND_DISPLAY".to_string(), None),
        ("GDK_BACKEND".to_string(), Some("x11".to_string())),
    ]
}

/// 逻辑尺寸换算：(widthPx / density, heightPx / density)，最小 1
pub fn calculate_logical_size(width_px: i32, height_px: i32, density: f32) -> (i32, i32) {
    if width_px <= 0 || height_px <= 0 || density <= 0.0 {
        return (0, 0);
    }
    let w = ((width_px as f32) / density) as i32;
    let h = ((height_px as f32) / density) as i32;
    (w.max(1), h.max(1))
}

/// 地址去敏：只留到路径，去掉 query 和 hash
pub fn redact_url(url: Option<&str>) -> String {
    let Some(u) = url else {
        return "null".to_string();
    };
    if let Some(cut) = u.find(['?', '#']) {
        format!("{}?…", &u[..cut])
    } else {
        u.to_string()
    }
}

