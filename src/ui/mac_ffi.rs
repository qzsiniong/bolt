//! 共享的 Objective-C 桥接小工具。
//!
//! 只用 `objc2` 0.6 的 raw `msg_send!`——不引入 `objc2-foundation` /
//! `objc2-app-kit`：它们目前依赖 objc2 0.5，会与本项目的 0.6 同时进入
//! 依赖图，`msg_send!` 的 trait 校验会失败。
//!
//! 这里的函数都是「取到值立刻拷贝成 Rust 类型、不留 Objective-C 引用」，
//! 所以调用方不需要管理内存（对象都是 autoreleased）。

use objc2::runtime::AnyObject;

/// 建一个 autoreleased 的 `NSString`（`+[NSString stringWithUTF8String:]`）。
/// 只在同一次调用里当类型/键名用，不长期持有。
pub(crate) fn ns_string(s: &str) -> *mut AnyObject {
    use objc2::msg_send;
    use objc2::runtime::AnyClass;
    use std::ffi::CString;

    let Ok(cstr) = CString::new(s) else {
        return std::ptr::null_mut();
    };
    let Some(cls) = AnyClass::get(c"NSString") else {
        return std::ptr::null_mut();
    };
    unsafe { msg_send![cls, stringWithUTF8String: cstr.as_ptr()] }
}

/// `NSData*` → `Vec<u8>`（立刻拷贝，不持有 Objective-C 对象）。
pub(crate) fn ns_data_bytes(data: *mut AnyObject) -> Option<Vec<u8>> {
    use objc2::msg_send;

    if data.is_null() {
        return None;
    }
    unsafe {
        let len: usize = msg_send![data, length];
        // `-[NSData bytes]` 的 ObjC 返回类型是 `const void *`，编码 `^v`；
        // objc2 在 debug 下会校验声明类型，声明成 `*const u8`（编码 `*`）会在
        // 运行时直接 abort：invalid message send ... expected '^v', but found '*'。
        // 所以必须先按 `*const c_void` 收，再自己转成 `*const u8`。
        let ptr: *const std::ffi::c_void = msg_send![data, bytes];
        if ptr.is_null() || len == 0 {
            return None;
        }
        Some(std::slice::from_raw_parts(ptr as *const u8, len).to_vec())
    }
}

/// `NSString*` → `String`（走 UTF8String，拷贝后即与 Objective-C 对象无关）。
pub(crate) fn ns_string_to_rust(nsstr: *mut AnyObject) -> Option<String> {
    use objc2::msg_send;

    if nsstr.is_null() {
        return None;
    }
    unsafe {
        let ptr: *const std::ffi::c_char = msg_send![nsstr, UTF8String];
        if ptr.is_null() {
            return None;
        }
        Some(std::ffi::CStr::from_ptr(ptr).to_string_lossy().into_owned())
    }
}

/// 执行 `/usr/bin/open`（`select` 为真时加 `-R` 选中而不是打开）。**阻塞**到
/// 子进程退出，所以只在工作线程里调——见 [`reveal_in_background`]。
fn run_open(path: &std::path::Path, select: bool) -> Result<(), String> {
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

/// 在工作线程里打开 / 选中路径，结果回调也在工作线程上执行。
///
/// **GPUI 回调里（点击处理器、`LauncherAction::run`）一律走这两个函数**，别
/// 直接 `std::process::Command`：
///
/// - 同步调用会卡住 UI 帧，直到 `open` 退出；
/// - 更要紧的是激活别的 App 这件事。曾经这里直接调
///   `-[NSWorkspace selectFile:inFileViewerRootedAtPath:]`，它在**当前线程同步
///   激活 Finder**；点按钮那一刻我们正处在 GPUI 的窗口事件更新里（App 已被
///   借用），AppKit 紧接着把 `windowDidResignKey` / `applicationDidResignActive`
///   同步派发回来，GPUI 再借一次 App → `RefCell already borrowed` 直接 abort。
///   工作线程里既没有 App 借用，也不占 UI 帧，激活回调落到哪都无所谓。
///
/// 回调里只能做线程安全的事：写 `ConsoleChannel`、推通知卡片都可以，别碰 `App`。
fn open_off_thread(
    path: std::path::PathBuf,
    select: bool,
    done: impl FnOnce(Result<(), String>) + Send + 'static,
) {
    std::thread::spawn(move || done(run_open(&path, select)));
}

/// 在 Finder 中**选中**路径（定位到它所在的那一层）：`open -R`。
pub(crate) fn reveal_in_background(
    path: std::path::PathBuf,
    done: impl FnOnce(Result<(), String>) + Send + 'static,
) {
    open_off_thread(path, true, done);
}

/// 复制文本到系统剪切板。
pub(crate) fn copy_text(text: &str) -> Result<(), String> {
    use objc2::msg_send;
    use objc2::runtime::AnyClass;

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

/// 通知消息（自绘卡片）。title 加粗在顶部，body 在下方。
///
/// 实际展示在 `ui/notify_window.rs`：只往 channel 推一条请求（app tick 每帧
/// drain 开窗），这里保持无 gpui 依赖，所有调用方签名不变。
pub(crate) fn notify(title: &str, body: &str) {
    crate::ui::notify_window::push_notify(title, body);
}

/// 用默认程序**打开**路径（目录就是开一个 Finder 窗口）。
///
/// 与 [`reveal_in_background`] 的区别：后者是"选中"，前者是"打开"。打开数据
/// 目录用它，因为用户要的是浏览内容，不是让父目录闪一下。
pub(crate) fn open_in_background(
    path: std::path::PathBuf,
    done: impl FnOnce(Result<(), String>) + Send + 'static,
) {
    open_off_thread(path, false, done);
}

/// `+[NSURL fileURLWithPath:]`。
///
/// 不用 `fileURLWithPath:isDirectory:`：那个 BOOL 形参的 ObjC 编码是 `c`，
/// Rust `bool` 是 `B`，objc2 在 debug 下会当场 abort。
fn ns_file_url(path: &std::path::Path) -> *mut AnyObject {
    use objc2::msg_send;
    use objc2::runtime::AnyClass;

    let Some(cls) = AnyClass::get(c"NSURL") else {
        return std::ptr::null_mut();
    };
    let ns_path = ns_string(&path.to_string_lossy());
    if ns_path.is_null() {
        return std::ptr::null_mut();
    }
    unsafe { msg_send![cls, fileURLWithPath: ns_path] }
}

/// 把路径移到系统废纸篓：`-[NSFileManager trashItemAtURL:resultingItemURL:error:]`。
///
/// 失败时**不**删——调用方负责退回 [`move_to_trash`] 或提示用户手动处理。
pub(crate) fn trash_item(path: &std::path::Path) -> Result<(), String> {
    use objc2::msg_send;
    use objc2::runtime::AnyClass;

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

/// 纯文件系统回退：把 `path` 挪进 `trash_dir`，重名时加 `-<unix 秒>` 后缀。
///
/// 与 [`trash_item`] 分开是为了可测：不依赖 AppKit，也不碰真实废纸篓。
pub(crate) fn move_to_trash(
    path: &std::path::Path,
    trash_dir: &std::path::Path,
) -> Result<std::path::PathBuf, String> {
    std::fs::create_dir_all(trash_dir)
        .map_err(|e| format!("创建 {} 失败：{e}", trash_dir.display()))?;
    let name = path
        .file_name()
        .ok_or_else(|| format!("路径没有文件名：{}", path.display()))?;
    let mut target = trash_dir.join(name);
    if target.exists() {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_secs())
            .unwrap_or_default();
        let mut candidate = std::ffi::OsString::from(name);
        candidate.push(format!("-{stamp}"));
        target = trash_dir.join(candidate);
        let mut index = 1;
        while target.exists() {
            let mut candidate = std::ffi::OsString::from(name);
            candidate.push(format!("-{stamp}-{index}"));
            target = trash_dir.join(candidate);
            index += 1;
        }
    }
    std::fs::rename(path, &target).map_err(|e| format!("移动到 {} 失败：{e}", target.display()))?;
    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TempDir;

    #[test]
    fn move_to_trash_moves_and_suffixes_on_conflict() {
        let tmp = TempDir::new("mac-ffi-trash");
        let trash = tmp.0.join("Trash");
        let plugin = tmp.0.join("demo-v1.0.0.js");
        std::fs::create_dir_all(&plugin).unwrap();
        std::fs::write(plugin.join("extension.json"), "{}").unwrap();

        let target = move_to_trash(&plugin, &trash).expect("first move");
        assert!(!plugin.exists());
        assert!(target.join("extension.json").is_file());

        // 同名再进来一次：不能覆盖废纸篓里已有的，要加后缀。
        std::fs::create_dir_all(&plugin).unwrap();
        let second = move_to_trash(&plugin, &trash).expect("second move");
        assert_ne!(second, target);
        assert!(second.is_dir());
        assert!(target.join("extension.json").is_file());
    }

    #[test]
    fn move_to_trash_reports_a_missing_source() {
        let tmp = TempDir::new("mac-ffi-trash-missing");
        let error =
            move_to_trash(&tmp.0.join("nope"), &tmp.0.join("Trash")).expect_err("must fail");
        assert!(!error.is_empty());
    }
}
