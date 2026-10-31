//! 窗口层级的唯一 FFI 出口。
//!
//! 「置顶」就是 NSWindow 的 `level` 属性：`NSFloatingWindowLevel = 3`、
//! `NSNormalWindowLevel = 0`。`WindowKind::Floating` 与 `Normal` 在 gpui-pre
//! （window.rs L1210-L1218）只差 `setLevel_` 与鼠标移动事件开关，两者都是
//! NSWindow 的运行时消息——层级并非「只能创建时确定」，切换无需重建视图。
//!
//! 本模块 `pub(super)`，外界只能经 [`super`](crate::ui::window) 的重导出使用，
//! 保证 objc2 消息不会从别的模块散出去。
//!
//! 非 macOS：置顶不受支持（`is_pinned` 恒 false、其余空实现），与旧的
//! `window_pin` 行为一致；本项目只在 macOS 上发布。

use gpui_kit::Window;

const FLOATING_LEVEL: i64 = 3;
const NORMAL_LEVEL: i64 = 0;

/// 读窗口当前层级。层级是「置顶与否」的唯一真相：[`super::WindowPin`] 渲染时
/// 读它、开窗侧设它，两边不会漂移。读不到（句柄失效等）返回 `None`。
#[cfg(target_os = "macos")]
pub(crate) fn window_level(window: &Window) -> Option<i64> {
    use objc2::msg_send;
    use objc2::runtime::AnyObject;
    use raw_window_handle::RawWindowHandle;

    let Ok(handle) = raw_window_handle::HasWindowHandle::window_handle(window) else {
        return None;
    };
    let RawWindowHandle::AppKit(appkit) = handle.as_raw() else {
        return None;
    };
    let ns_view = appkit.ns_view.as_ptr() as *mut AnyObject;
    unsafe {
        let ns_window: *mut AnyObject = msg_send![ns_view, window];
        if ns_window.is_null() {
            return None;
        }
        // NSWindow.level 返回 NSInteger（64 位平台 = long，编码 "q"）。
        let level: i64 = msg_send![ns_window, level];
        Some(level)
    }
}

#[cfg(target_os = "macos")]
pub(crate) fn set_level(window: &Window, level: i64) {
    use objc2::msg_send;
    use objc2::runtime::AnyObject;
    use raw_window_handle::RawWindowHandle;

    let Ok(handle) = raw_window_handle::HasWindowHandle::window_handle(window) else {
        return;
    };
    let RawWindowHandle::AppKit(appkit) = handle.as_raw() else {
        return;
    };
    let ns_view = appkit.ns_view.as_ptr() as *mut AnyObject;
    unsafe {
        let ns_window: *mut AnyObject = msg_send![ns_view, window];
        if ns_window.is_null() {
            return;
        }
        let _: () = msg_send![ns_window, setLevel: level];
        // gpui 只在开 `WindowKind::Floating` 时打开鼠标移动事件；层级升到
        // floating 时同样打开，落回 normal 时关掉，与 gpui 的行为对齐。
        let _: () = msg_send![ns_window, setAcceptsMouseMovedEvents: level != 0];
    }
}

/// 置顶 / 取消置顶。与 `WindowKind::Floating` 同级，运行时切换不关窗。
pub(crate) fn set_pinned(window: &Window, pinned: bool) {
    #[cfg(target_os = "macos")]
    set_level(window, if pinned { FLOATING_LEVEL } else { NORMAL_LEVEL });
    #[cfg(not(target_os = "macos"))]
    let _ = (window, pinned);
}

/// 当前是否置顶：**唯一真相 = 窗口真实层级**。
pub(crate) fn is_pinned(window: &Window) -> bool {
    #[cfg(target_os = "macos")]
    return window_level(window).is_some_and(|level| level >= FLOATING_LEVEL);
    #[cfg(not(target_os = "macos"))]
    {
        let _ = window;
        false
    }
}

/// 沿 gpui 坐标系（y 向下）把窗口整体平移 `(dx, dy)` 像素。
///
/// gpui 没有移动窗口原点的 API（`resize` 在 macOS 上锚定左下角），「缩放时
/// 保持光标下的内容不动」这类需求只能直发 NSWindow 消息。`setFrameOrigin`
/// 用 NS 坐标系（y 向上），所以 y 位移取反；frame 整体读出来再改 origin。
#[cfg(target_os = "macos")]
pub(crate) fn move_origin(window: &Window, dx: f64, dy: f64) {
    use objc2::encode::Encode as _;
    use objc2::msg_send;
    use objc2::runtime::AnyObject;
    use raw_window_handle::RawWindowHandle;

    // 名字必须与 AppKit 方法签名里的类型编码一致：NSWindow.frame 返回的是
    // **CGRect/CGPoint/CGSize**（NSRect 只是 typedef 别名，objc2 校验的是
    // 编码字符串里的结构体名，写 NSRect 会 panic）。
    #[repr(C)]
    struct CGPoint {
        x: f64,
        y: f64,
    }
    #[repr(C)]
    struct CGSize {
        width: f64,
        height: f64,
    }
    #[repr(C)]
    struct CGRect {
        origin: CGPoint,
        size: CGSize,
    }
    // objc2 的 msg_send 要求参与编解码的结构体实现 Encode/RefEncode。
    unsafe impl objc2::encode::Encode for CGPoint {
        const ENCODING: objc2::encode::Encoding =
            objc2::encode::Encoding::Struct("CGPoint", &[f64::ENCODING, f64::ENCODING]);
    }
    unsafe impl objc2::encode::RefEncode for CGPoint {
        const ENCODING_REF: objc2::encode::Encoding =
            objc2::encode::Encoding::Pointer(&Self::ENCODING);
    }
    unsafe impl objc2::encode::Encode for CGSize {
        const ENCODING: objc2::encode::Encoding =
            objc2::encode::Encoding::Struct("CGSize", &[f64::ENCODING, f64::ENCODING]);
    }
    unsafe impl objc2::encode::RefEncode for CGSize {
        const ENCODING_REF: objc2::encode::Encoding =
            objc2::encode::Encoding::Pointer(&Self::ENCODING);
    }
    unsafe impl objc2::encode::Encode for CGRect {
        const ENCODING: objc2::encode::Encoding =
            objc2::encode::Encoding::Struct("CGRect", &[CGPoint::ENCODING, CGSize::ENCODING]);
    }
    unsafe impl objc2::encode::RefEncode for CGRect {
        const ENCODING_REF: objc2::encode::Encoding =
            objc2::encode::Encoding::Pointer(&Self::ENCODING);
    }

    let Ok(handle) = raw_window_handle::HasWindowHandle::window_handle(window) else {
        return;
    };
    let RawWindowHandle::AppKit(appkit) = handle.as_raw() else {
        return;
    };
    let ns_view = appkit.ns_view.as_ptr() as *mut AnyObject;
    unsafe {
        let ns_window: *mut AnyObject = msg_send![ns_view, window];
        if ns_window.is_null() {
            return;
        }
        let frame: CGRect = msg_send![ns_window, frame];
        let _: () = msg_send![ns_window,
            setFrameOrigin: CGPoint {
                x: frame.origin.x + dx,
                y: frame.origin.y - dy,
            }
        ];
    }
}

/// 非 macOS 平台暂不支持，空实现。
#[cfg(not(target_os = "macos"))]
pub(crate) fn move_origin(_window: &Window, _dx: f64, _dy: f64) {}
