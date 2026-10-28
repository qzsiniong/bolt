//! 窗口置顶（图钉）：宿主公共能力，任何原生窗口都能直接复用。
//!
//! 置顶**不关窗重开**：`WindowKind::Floating` 与 `Normal` 在 gpui-pre-macos
//! （window.rs L1210-L1218）只差 `setLevel_` 与鼠标移动事件开关，两者都是
//! NSWindow 的运行时消息——窗口层级并非「只能创建时确定」，切换无需重建视图。
//!
//! 三步用法：
//! 1. 开窗时用 `TitleBar::window_options()` 当 `WindowOptions` 的基底（自绘标题栏
//!    自己处理拖动与双击）；清单声明置顶的窗口在开窗成功后调一次
//!    [`set_window_level`]，把层级抬到 floating。
//! 2. 视图里持有 [`WindowPin`]：`cx.new(|_| WindowPin::new(pinned))`。
//! 3. 渲染把它放进 `TitleBar` 右侧：`TitleBar::new().child(pin.clone())`；
//!    点击即翻转状态并同步窗口层级，不需要调用方做任何事。

use gpui_kit::assets::IconName;
use gpui_kit::component::Sizable as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::*;

/// 直接改 NSWindow 层级实现置顶 / 取消置顶。
///
/// 经 `Window: HasWindowHandle`（gpui-pre window.rs L7571）拿到
/// `AppKitWindowHandle` 的 NSView，再向它要 NSWindow 发消息；`NSFloatingWindowLevel
/// = 3`、`NSNormalWindowLevel = 0`。gpui 自身不读窗口层级，改层级没有副作用。
#[cfg(target_os = "macos")]
pub(crate) fn set_window_level(window: &Window, pinned: bool) {
    use objc2::msg_send;
    use objc2::runtime::AnyObject;
    use raw_window_handle::RawWindowHandle;

    // gpui 的 `Window` 自带固有 `window_handle()`（返回 `AnyWindowHandle`），
    // 会遮蔽 trait 方法，这里显式走 `HasWindowHandle` 拿 raw handle。
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
        // NSFloatingWindowLevel = 3，NSNormalWindowLevel = 0。
        let _: () = msg_send![ns_window, setLevel: if pinned { 3i64 } else { 0i64 }];
        let _: () = msg_send![ns_window, setAcceptsMouseMovedEvents: pinned];
    }
}

/// 非 macOS 平台暂不支持置顶，保持空实现以让调用方无需 `cfg`。
#[cfg(not(target_os = "macos"))]
pub(crate) fn set_window_level(_window: &Window, _pinned: bool) {}

/// 沿 gpui 坐标系（y 向下）把窗口整体平移 `(dx, dy)` 像素。
///
/// gpui 没有移动窗口原点的 API（`resize` 在 macOS 上锚定左下角），「缩放时
/// 保持光标下的内容不动」这类需求只能直发 NSWindow 消息。`setFrameOrigin`
/// 用 NS 坐标系（y 向上），所以 y 位移取反；frame 整体读出来再改 origin。
#[cfg(target_os = "macos")]
pub(crate) fn move_window_origin(window: &Window, dx: f64, dy: f64) {
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
pub(crate) fn move_window_origin(_window: &Window, _dx: f64, _dy: f64) {}

/// 标题栏右上角的置顶图钉：自持 `pinned` 状态，点击翻转并同步窗口层级。
///
/// 状态存在实体里而不是父视图字段里，是为了让「按钮 + 状态 + 层级同步」整块
/// 可复用：调用方只需要 `cx.new` 一个实例、把它挂到 `TitleBar` 上。
pub(crate) struct WindowPin {
    pinned: bool,
}

impl WindowPin {
    /// `pinned` 要与开窗时的层级一致——默认置顶的窗口先调过
    /// [`set_window_level`] 再把它作为初值传进来。
    pub(crate) fn new(pinned: bool) -> Self {
        Self { pinned }
    }
}

impl Render for WindowPin {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (icon, tooltip) = if self.pinned {
            (IconName::PinOff, "取消置顶")
        } else {
            (IconName::Pin, "置顶")
        };
        // 元素 id 由「固定前缀 + 本实体 id」组成：同一窗口里放多个图钉也不会撞 id。
        let id = ElementId::from(("window-pin", cx.entity_id()));
        div()
            .id(id.clone())
            .flex()
            .items_center()
            .justify_end()
            .px_2()
            // 自绘标题栏整条都是拖动区，按下事件要拦下来，否则点图钉会变成拖窗。
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                Button::new(id)
                    .icon(icon)
                    .small()
                    .ghost()
                    .tooltip(tooltip)
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.pinned = !this.pinned;
                        set_window_level(window, this.pinned);
                        cx.notify();
                    })),
            )
    }
}
