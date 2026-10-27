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

/// 在 Finder 中显示（reveal）指定路径：`-[NSWorkspace selectFile:...]`。
pub(crate) fn reveal_in_finder(path: &std::path::Path) -> Result<(), String> {
    use objc2::msg_send;
    use objc2::runtime::AnyClass;

    let cls = AnyClass::get(c"NSWorkspace").ok_or("NSWorkspace 类不可用")?;
    let workspace: *mut AnyObject = unsafe { msg_send![cls, sharedWorkspace] };
    if workspace.is_null() {
        return Err("NSWorkspace.sharedWorkspace 不可用".to_string());
    }
    let ns_path = ns_string(&path.to_string_lossy());
    if ns_path.is_null() {
        return Err(format!("路径 {} 转 NSString 失败", path.display()));
    }
    // 第二个参数传 nil（rootFullPath 不用）。
    let ok: bool = unsafe {
        msg_send![workspace, selectFile: ns_path, inFileViewerRootedAtPath: std::ptr::null_mut::<AnyObject>()]
    };
    if ok {
        Ok(())
    } else {
        Err(format!("Finder 无法显示 {}", path.display()))
    }
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
