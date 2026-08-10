//! CLI 彩色输出：`--color` / `--no-color` / 自动检测 TTY。

use std::env;
use std::io::{self, IsTerminal};
use std::sync::atomic::{AtomicBool, Ordering};

static COLOR_ENABLED: AtomicBool = AtomicBool::new(false);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorChoice {
    Auto,
    Always,
    Never,
}

impl ColorChoice {
    pub fn from_flag(s: &str) -> Option<Self> {
        match s {
            "auto" => Some(Self::Auto),
            "always" | "on" | "true" | "yes" => Some(Self::Always),
            "never" | "off" | "false" | "no" => Some(Self::Never),
            _ => None,
        }
    }

    pub fn resolve(self) -> bool {
        match self {
            Self::Always => true,
            Self::Never => false,
            Self::Auto => auto_color_preferred(),
        }
    }
}

fn auto_color_preferred() -> bool {
    if env::var_os("NO_COLOR").is_some() {
        return false;
    }
    if env::var_os("FORCE_COLOR").is_some() || env::var_os("CLICOLOR_FORCE").is_some() {
        return true;
    }
    if env::var("TERM").ok().as_deref() == Some("dumb") {
        return false;
    }
    // 任一侧是终端即允许；提示符走 stdout，Error 走 stderr。
    io::stdout().is_terminal() || io::stderr().is_terminal()
}

/// 应用颜色策略；在 Always/Auto 且可用时尝试启用 Windows VT。
pub fn init(choice: ColorChoice) {
    let on = choice.resolve();
    if on {
        enable_windows_vt();
    }
    COLOR_ENABLED.store(on, Ordering::Relaxed);
}

pub fn enabled() -> bool {
    COLOR_ENABLED.load(Ordering::Relaxed)
}

const RESET: &str = "\x1b[0m";
const BRIGHT_PURPLE: &str = "\x1b[95m";
const RED: &str = "\x1b[31m";
const GREEN: &str = "\x1b[32m";
const CYAN: &str = "\x1b[36m";
const DIM: &str = "\x1b[2m";

fn paint(code: &str, text: &str) -> String {
    if enabled() {
        format!("{code}{text}{RESET}")
    } else {
        text.to_string()
    }
}

pub fn purple(text: &str) -> String {
    paint(BRIGHT_PURPLE, text)
}

pub fn red(text: &str) -> String {
    paint(RED, text)
}

pub fn green(text: &str) -> String {
    paint(GREEN, text)
}

pub fn cyan(text: &str) -> String {
    paint(CYAN, text)
}

pub fn dim(text: &str) -> String {
    paint(DIM, text)
}

/// 状态行：两格缩进 + 绿色（如 Project / Running）。
pub fn status_line(text: &str) {
    println!("{}", green(&format!("  {text}")));
}

pub fn eprint_error(msg: impl AsRef<str>) {
    let msg = msg.as_ref();
    eprintln!("{}", red(msg));
}

/// 从 argv 抽出颜色开关，返回 (choice, 剩余参数含 program name)。
pub fn take_color_args(args: &[String]) -> (ColorChoice, Vec<String>) {
    let mut choice = ColorChoice::Auto;
    let mut out = Vec::with_capacity(args.len());
    if let Some(first) = args.first() {
        out.push(first.clone());
    }
    let mut i = 1;
    while i < args.len() {
        let a = args[i].as_str();
        if a == "--no-color" {
            choice = ColorChoice::Never;
            i += 1;
            continue;
        }
        if let Some(rest) = a.strip_prefix("--color=") {
            if let Some(c) = ColorChoice::from_flag(rest) {
                choice = c;
                i += 1;
                continue;
            }
            // 未知值：当作普通参数留下
            out.push(args[i].clone());
            i += 1;
            continue;
        }
        if a == "--color" {
            if i + 1 < args.len() {
                if let Some(c) = ColorChoice::from_flag(args[i + 1].as_str()) {
                    choice = c;
                    i += 2;
                    continue;
                }
            }
            choice = ColorChoice::Always;
            i += 1;
            continue;
        }
        out.push(args[i].clone());
        i += 1;
    }
    (choice, out)
}

#[cfg(windows)]
fn enable_windows_vt() {
    // ENABLE_VIRTUAL_TERMINAL_PROCESSING
    const ENABLE_VIRTUAL_TERMINAL_PROCESSING: u32 = 0x0004;
    const STD_OUTPUT_HANDLE: i32 = -11;
    const STD_ERROR_HANDLE: i32 = -12;

    #[link(name = "kernel32")]
    extern "system" {
        fn GetStdHandle(n_std_handle: i32) -> *mut std::ffi::c_void;
        fn GetConsoleMode(h_console: *mut std::ffi::c_void, lp_mode: *mut u32) -> i32;
        fn SetConsoleMode(h_console: *mut std::ffi::c_void, dw_mode: u32) -> i32;
    }

    unsafe {
        for handle_id in [STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
            let h = GetStdHandle(handle_id);
            if h.is_null() || h == (-1isize as *mut _) {
                continue;
            }
            let mut mode = 0u32;
            if GetConsoleMode(h, &raw mut mode) != 0 {
                let _ = SetConsoleMode(h, mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING);
            }
        }
    }
}

#[cfg(not(windows))]
fn enable_windows_vt() {}

#[cfg(test)]
mod tests {
    use super::*;

#[test]
fn take_color_flags() {
    let args = vec![
        "Optive".into(),
        "--color".into(),
        "run".into(),
        ".".into(),
    ];
    let (c, rest) = take_color_args(&args);
    assert_eq!(c, ColorChoice::Always);
    assert_eq!(rest, vec!["Optive", "run", "."]);

    let args = vec![
        "Optive".into(),
        "--no-color".into(),
        "new".into(),
        "App".into(),
    ];
    let (c, rest) = take_color_args(&args);
    assert_eq!(c, ColorChoice::Never);
    assert_eq!(rest, vec!["Optive", "new", "App"]);

    let args = vec!["Optive".into(), "--color=auto".into()];
    let (c, _) = take_color_args(&args);
    assert_eq!(c, ColorChoice::Auto);

    let args = vec![
        "Optive".into(),
        "--color".into(),
        "never".into(),
        "run".into(),
    ];
    let (c, rest) = take_color_args(&args);
    assert_eq!(c, ColorChoice::Never);
    assert_eq!(rest, vec!["Optive", "run"]);
}
}
