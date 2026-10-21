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
