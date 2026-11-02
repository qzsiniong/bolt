//! macOS 平台实现。
//!
//! 打开 / 选中走 `/usr/bin/open`（AppKit 的 `NSWorkspace` 会在当前线程同步激活
//! 目标 App，见 `super::off_thread` 的注释）；剪贴板与废纸篓直接发原始
//! Objective-C 消息——不引入 `objc2-app-kit`，它依赖 objc2 0.5，会与项目的
//! 0.6 同时进依赖图，`msg_send!` 的类型校验当场失败。

use std::path::{Path, PathBuf};

use crate::ui::mac_ffi::{ns_file_url, ns_string, ns_string_to_rust};

/// 执行 `/usr/bin/open`（`select` 为真时加 `-R` 选中而不是打开）。**阻塞**到
/// 子进程退出，所以只在工作线程里调——见 [`super::open_in_background`]。
fn run_open(path: &Path, select: bool) -> Result<(), String> {
    if !path.exists() {
        return Err(format!("路径不存在：{}", path.display()));
    }
    let mut cmd = std::process::Command::new("/usr/bin/open");
    if select {
        cmd.arg("-R");
    }
    let status = cmd
        .arg(path)
        .status()
        .map_err(|e| format!("调用 open 失败：{e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("open 返回 {status}：{}", path.display()))
    }
}

pub(super) fn open_path(path: &Path) -> Result<(), String> {
    run_open(path, false)
}

pub(super) fn reveal_path(path: &Path) -> Result<(), String> {
    run_open(path, true)
}

/// 复制文本到系统剪贴板。
pub(super) fn copy_text(text: &str) -> Result<(), String> {
    use objc2::msg_send;
    use objc2::runtime::{AnyClass, AnyObject};

    let cls = AnyClass::get(c"NSPasteboard").ok_or("NSPasteboard 类不可用")?;
    let pb: *mut AnyObject = unsafe { msg_send![cls, generalPasteboard] };
    if pb.is_null() {
        return Err("NSPasteboard.generalPasteboard 不可用".to_string());
    }
    let ns_text = ns_string(text);
    let ns_type = ns_string("public.utf8-plain-text");
    if ns_text.is_null() || ns_type.is_null() {
        return Err("NSString 创建失败".to_string());
    }
    unsafe {
        let _: isize = msg_send![pb, clearContents];
        let ok: bool = msg_send![pb, setString: ns_text, forType: ns_type];
        if ok {
            Ok(())
        } else {
            Err("setString:forType: 返回 NO".to_string())
        }
    }
}

/// 把路径移到系统废纸篓：`-[NSFileManager trashItemAtURL:resultingItemURL:error:]`。
pub(super) fn trash_path(path: &Path) -> Result<(), String> {
    use objc2::msg_send;
    use objc2::runtime::{AnyClass, AnyObject};

    let cls = AnyClass::get(c"NSFileManager").ok_or("NSFileManager 类不可用")?;
    let manager: *mut AnyObject = unsafe { msg_send![cls, defaultManager] };
    if manager.is_null() {
        return Err("NSFileManager.defaultManager 不可用".to_string());
    }
    let url = ns_file_url(path);
    if url.is_null() {
        return Err(format!("路径 {} 转 NSURL 失败", path.display()));
    }

    // 两个出参的编码都是 `^@`（指向对象指针的指针）。传 `null_mut()`（编码 `@`）
    // 会在 objc2 的类型校验里 abort，所以必须给出真实的可写槽位。
    let mut resulting: *mut AnyObject = std::ptr::null_mut();
    let mut error: *mut AnyObject = std::ptr::null_mut();
    let ok: bool = unsafe {
        msg_send![
            manager,
            trashItemAtURL: url,
            resultingItemURL: &mut resulting,
            error: &mut error
        ]
    };
    if ok {
        return Ok(());
    }
    let reason = ns_string_to_rust(unsafe { msg_send![error, localizedDescription] })
        .filter(|text| !text.is_empty())
        .unwrap_or_else(|| "trashItemAtURL 返回 NO".to_string());
    Err(reason)
}

pub(super) fn trash_dir() -> Option<PathBuf> {
    dirs::home_dir().map(|home| home.join(".Trash"))
}

/// AppKit 全局坐标：主屏左下原点、y 向上。
pub(super) fn window_top_left(x: f64, y: f64) -> (f64, f64) {
    let primary_h = core_graphics::display::CGDisplay::main()
        .bounds()
        .size
        .height;
    (x, primary_h - y)
}

/// 三处标准应用目录。
pub(super) fn app_roots() -> Vec<(PathBuf, &'static str)> {
    let mut roots = vec![
        (PathBuf::from("/Applications"), "应用"),
        (PathBuf::from("/System/Applications"), "系统应用"),
    ];
    if let Some(home) = dirs::home_dir() {
        roots.push((home.join("Applications"), "用户应用"));
    }
    roots
}

pub(super) fn shell_command(cmd: &str) -> (String, Vec<String>) {
    ("sh".to_string(), vec!["-c".to_string(), cmd.to_string()])
}
