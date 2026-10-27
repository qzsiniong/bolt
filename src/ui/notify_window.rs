//! 自绘通知卡片：替代 osascript 系统横幅，显示在鼠标所在屏幕的底部中间。
//!
//! `mac_ffi::notify` 只往 channel 推一条请求（无 gpui 依赖，调用方签名不变），
//! boot.rs 的 app tick 每帧 [`drain_notifications`] 把请求开成窗口。多个通知
//! 纵向堆叠（新通知追加在下），hover 暂停倒计时，点击立即关闭；任一通知关闭
//! 后，位于其下方的通知整体上移补位（NSWindow setFrameTopLeftPoint 平移）。
//!
//! 窗口用 `kind: PopUp`（NonactivatingPanel）：App 非激活时也能显示且不抢焦点；
//! 透明背景 + 框架层 `cx.open_window` 裸开（绕开 gpui-kit Root 的垫白插件），
//! 卡片圆角由 div 自绘。

use std::cell::Cell;
use std::sync::OnceLock;
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender};
use gpui_kit::assets::IconName;
use gpui_kit::base::v_flex;
use gpui_kit::component::{ActiveTheme, Icon};
use gpui_kit::*;

use crate::app::shell::AppShell;

/// 卡片固定尺寸（正文单行截断）、堆叠间距、距屏底边距、显示时长与倒计时步长。
const NOTIFY_W: f32 = 340.;
const NOTIFY_H: f32 = 64.;
const STACK_GAP: f32 = 8.;
const BOTTOM_MARGIN: f32 = 100.;
const SHOW_DURATION: Duration = Duration::from_secs(5);
const TICK: Duration = Duration::from_millis(100);

/// 一条待显示的通知：mac_ffi::notify 推进 channel，drain 时开窗。
#[derive(Clone)]
pub(crate) struct NotifyRequest {
    pub(crate) title: String,
    pub(crate) body: String,
}

static NOTIFY_TX: OnceLock<Sender<NotifyRequest>> = OnceLock::new();
static NOTIFY_RX: OnceLock<Receiver<NotifyRequest>> = OnceLock::new();

/// boot.rs 启动时调用一次：建好通知通道。
pub(crate) fn init_channel() {
    let (tx, rx) = crossbeam_channel::unbounded();
    let _ = NOTIFY_TX.set(tx);
    let _ = NOTIFY_RX.set(rx);
}

/// 通知入口（mac_ffi::notify 转发到这里）：只入队，不阻塞、不报错——
/// channel 未初始化（App 尚未启动完）时通知丢弃，与旧实现的丢错行为一致。
pub(crate) fn push_notify(title: &str, body: &str) {
    if let Some(tx) = NOTIFY_TX.get() {
        let _ = tx.send(NotifyRequest {
            title: title.to_string(),
            body: body.to_string(),
        });
    }
}

/// app tick 每帧调用：把排队的通知逐条开窗显示。
pub(crate) fn drain_notifications(cx: &mut App) {
    let Some(rx) = NOTIFY_RX.get() else { return };
    while let Ok(req) = rx.try_recv() {
        show_notify(cx, req);
    }
}

/// 一张通知卡片。
///
/// `top_left` 记窗口在 AppKit 全局坐标系的顶左点（gpui 没有窗口移动 API，
/// 关闭补位的上移靠它 + `setFrameTopLeftPoint:` 实现，不读 NSRect——那需要
/// 手写 objc2 的 Decode）。
struct NotifyCard {
    title: String,
    body: String,
    hovering: bool,
    top_left: Cell<(f64, f64)>,
    _tick: Task<()>,
}

impl NotifyCard {
    fn new(
        req: NotifyRequest,
        appkit_top_left: (f64, f64),
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let handle = window.window_handle();
        // 倒计时：累计「未 hover」的时间，达 SHOW_DURATION 自动关闭；view 已
        // 释放（窗口被关）则退出循环。
        let tick = cx.spawn(async move |this, cx| {
            let mut elapsed = Duration::ZERO;
            loop {
                cx.background_executor().timer(TICK).await;
                let Ok(hovering) = this.update(cx, |this, _| this.hovering) else {
                    return;
                };
                if !hovering {
                    elapsed += TICK;
                    if elapsed >= SHOW_DURATION {
                        break;
                    }
                }
            }
            cx.update(|cx| close_notify(handle, cx, None));
        });
        Self {
            title: req.title,
            body: req.body,
            hovering: false,
            top_left: Cell::new(appkit_top_left),
            _tick: tick,
        }
    }

    /// 上移一格补位（AppKit y 向上，即 y += 格高）。
    fn shift_up(&self, window: &Window) {
        let next = (
            self.top_left.get().0,
            self.top_left.get().1 + (NOTIFY_H + STACK_GAP) as f64,
        );
        self.top_left.set(next);
        set_frame_top_left(window, next);
    }
}

impl Render for NotifyCard {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("notify-card")
            .w(px(NOTIFY_W))
            .h(px(NOTIFY_H))
            .flex()
            .items_center()
            .gap(px(12.))
            .px(px(14.))
            .bg(cx.theme().popover)
            .border_1()
            .border_color(cx.theme().border)
            .rounded_xl()
            .shadow_lg()
            .occlude()
            .on_hover(cx.listener(|this, hovering: &bool, _, _cx| {
                // 只翻转状态，不 cx.notify()：hover 没有视觉反馈，重绘会重建
                // NSTrackingArea 并立刻补发一次假的 enter（「离开后马上 true」
                // 的来源），把 hover 永久卡在 true、倒计时永远暂停。
                this.hovering = *hovering;
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|_this, _, window, cx| {
                    close_notify(window.window_handle(), cx, Some(window));
                }),
            )
            .child(
                div()
                    .flex_none()
                    .size(px(32.))
                    .rounded_full()
                    .bg(cx.theme().accent)
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(Icon::new(IconName::Zap).size_4().text_color(rgb(0xFFFFFF))),
            )
            .child(
                v_flex()
                    .flex_1()
                    .overflow_hidden()
                    .gap(px(2.))
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(cx.theme().popover_foreground)
                            .truncate()
                            .child(self.title.clone()),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .truncate()
                            .child(self.body.clone()),
                    ),
            )
    }
}

/// 开一张通知卡片窗口：鼠标所在屏底部中间，按已有通知数向下堆叠。
fn show_notify(cx: &mut App, req: NotifyRequest) {
    let Some((display_id, (mx, my, mw, mh))) = cursor_screen_frame() else {
        return;
    };
    let index = cx.global::<AppShell>().notify_windows.len();
    let local_x = ((mw - NOTIFY_W as f64) / 2.).max(0.);
    let local_y =
        mh - BOTTOM_MARGIN as f64 - NOTIFY_H as f64 - index as f64 * (NOTIFY_H + STACK_GAP) as f64;
    let options = WindowOptions {
        // origin 相对指定 display 的本地坐标（gpui 负责转 AppKit）。
        window_bounds: Some(WindowBounds::Windowed(Bounds::new(
            point(px(local_x as f32), px(local_y as f32)),
            size(px(NOTIFY_W), px(NOTIFY_H)),
        ))),
        display_id: Some(display_id),
        titlebar: None,
        // NonactivatingPanel：App 非激活时也能显示，不抢焦点。
        kind: WindowKind::PopUp,
        is_resizable: false,
        focus: false,
        window_background: WindowBackgroundAppearance::Transparent,
        ..Default::default()
    };
    let cg_top_left = (mx + local_x, my + local_y);
    // AppKit 全局顶左点（主屏左下原点，y 向上）：顶左 y = 主屏高 - cg_y。
    let primary_h = core_graphics::display::CGDisplay::main()
        .bounds()
        .size
        .height;
    let appkit_top_left = (cg_top_left.0, primary_h - cg_top_left.1);
    match cx.open_window(options, |window, cx| {
        cx.new(|cx| NotifyCard::new(req, appkit_top_left, window, cx))
    }) {
        Ok(handle) => {
            // NSPanel 路径的 frame 兜底：转 borderless（解除 constrainFrameRect）、
            // 去系统阴影（阴影由卡片 CSS shadow_lg 提供）、按算好的位置重定位。
            handle
                .update(cx, |_, window, _| finalize_window(window, appkit_top_left))
                .ok();
            cx.global_mut::<AppShell>()
                .notify_windows
                .push(handle.into());
        }
        Err(e) => {
            cx.global::<AppShell>()
                .console_channel
                .warn(format!("无法打开通知窗口：{e}"));
        }
    }
}

/// 关闭一张通知并把其下方的通知上移一格。
///
/// `window` 为 Some 表示从该窗口自己的回调里调用（不能用 handle 重入 update
/// 触发 double_lease_panic，自己的 remove_window 由这里顺手做）；None 表示从
/// app 层（倒计时 spawn）调用。
fn close_notify(closing: AnyWindowHandle, cx: &mut App, window: Option<&mut Window>) {
    let shell = cx.global_mut::<AppShell>();
    let Some(index) = shell.notify_windows.iter().position(|&h| h == closing) else {
        // 已不在堆叠里（重复触发）：只确保窗口本身关掉。
        match window {
            Some(w) => w.remove_window(),
            None => {
                let _ = closing.update(cx, |_, w, _| w.remove_window());
            }
        }
        return;
    };
    shell.notify_windows.remove(index);
    // 其下方的通知（index 之后的）整体上移一格补位。
    let below: Vec<AnyWindowHandle> = shell.notify_windows[index..].to_vec();
    for handle in below {
        let _ = handle.update(cx, |view, window, cx| {
            if let Ok(card) = view.downcast::<NotifyCard>() {
                card.update(cx, |this, _| this.shift_up(window));
            }
        });
    }
    match window {
        Some(w) => w.remove_window(),
        None => {
            let _ = closing.update(cx, |_, w, _| w.remove_window());
        }
    }
}

/// 鼠标所在显示器：`DisplayId` + CG 全局 frame (x, y, w, h)（主屏左上原点，
/// y 向下）。任一步失败回退主屏。
#[cfg(target_os = "macos")]
fn cursor_screen_frame() -> Option<(DisplayId, (f64, f64, f64, f64))> {
    use core_graphics::display::{
        CGDirectDisplayID, CGDisplay, CGDisplayBounds, CGGetActiveDisplayList,
    };
    use core_graphics::event::CGEvent;
    use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};

    let main = CGDisplay::main();
    let fallback = {
        let b = main.bounds();
        (
            DisplayId::new(main.id as u64),
            (b.origin.x, b.origin.y, b.size.width, b.size.height),
        )
    };
    let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState).ok()?;
    let point = CGEvent::new(source).ok()?;
    let p = point.location();

    let mut ids: Vec<CGDirectDisplayID> = Vec::with_capacity(32);
    let mut count = 0;
    // SAFETY: 缓冲区容量 32，count 写回实际数量。
    if unsafe { CGGetActiveDisplayList(ids.capacity() as u32, ids.as_mut_ptr(), &mut count) } != 0 {
        return Some(fallback);
    }
    unsafe { ids.set_len(count as usize) };

    ids.into_iter()
        .map(|id| (id, unsafe { CGDisplayBounds(id) }))
        .find(|(_, f)| {
            p.x >= f.origin.x
                && p.x < f.origin.x + f.size.width
                && p.y >= f.origin.y
                && p.y < f.origin.y + f.size.height
        })
        .map(|(id, f)| {
            (
                DisplayId::new(id as u64),
                (f.origin.x, f.origin.y, f.size.width, f.size.height),
            )
        })
        .or(Some(fallback.0).map(|id| (id, fallback.1)))
}

#[cfg(not(target_os = "macos"))]
fn cursor_screen_frame() -> Option<(DisplayId, (f64, f64, f64, f64))> {
    None
}

/// 开窗收尾：borderless（解除 NSPanel 的 constrainFrameRect）、去系统阴影、
/// 按 AppKit 全局顶左坐标重定位（与 view 记录的 `top_left` 一致）。
#[cfg(target_os = "macos")]
fn finalize_window(window: &Window, appkit_top_left: (f64, f64)) {
    set_frame_top_left(window, appkit_top_left);

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
        // NSBorderlessWindowMask = 0。
        let _: () = msg_send![ns_window, setStyleMask: 0usize];
        let _: () = msg_send![ns_window, setHasShadow: false];
    }
}

#[cfg(not(target_os = "macos"))]
fn finalize_window(_window: &Window, _appkit_top_left: (f64, f64)) {}

/// 按 AppKit 全局顶左坐标设置窗口位置。
#[cfg(target_os = "macos")]
fn set_frame_top_left(window: &Window, top_left: (f64, f64)) {
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
        let _: () = msg_send![ns_window, setFrameTopLeftPoint: NSPoint {
            x: top_left.0,
            y: top_left.1,
        }];
    }
}

#[cfg(not(target_os = "macos"))]
fn set_frame_top_left(_window: &Window, _top_left: (f64, f64)) {}

/// AppKit 的 NSPoint（= CGPoint）：msg_send 按值传结构体需要本地 `#[repr(C)]`
/// 同布局类型 + `Encode`（color_picker 同款做法）。
#[cfg(target_os = "macos")]
#[repr(C)]
#[derive(Clone, Copy)]
struct NSPoint {
    x: f64,
    y: f64,
}

#[cfg(target_os = "macos")]
unsafe impl objc2::encode::Encode for NSPoint {
    const ENCODING: objc2::encode::Encoding =
        objc2::encode::Encoding::Struct("CGPoint", &[f64::ENCODING; 2]);
}
