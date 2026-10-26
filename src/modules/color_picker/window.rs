//! 屏幕取色浮窗：整屏定格快照做背景 + 像素网格放大镜，点击拾色复制 HEX。
//!
//! 每块显示器各开一个窗口，铺满该屏并以截屏瞬间的画面定格；放大镜面板跟随
//! 光标，网格里每一格对应快照的一个物理像素（`cell = 逻辑坐标 × scale`），
//! 中心格即拾取目标。左键拾色、右键 / Esc 取消，整组浮窗一起关闭。

use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::ActiveTheme;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use image::Rgba;
use std::sync::Arc;

use crate::app::shell::AppShell;
use crate::modules::color_picker::capture::{self, ScreenSnapshot};
use crate::ui::mac_ffi::{copy_text, notify};

/// 网格边长（奇数，中心格 = 光标下的像素）：滚轮可调范围与默认起点。
const GRID_MIN: usize = 15;
const GRID_MAX: usize = 51;
const GRID: usize = 15;
const CELL: f32 = 12.;
const GAP: f32 = 2.;
const PAD: f32 = 8.;
/// 面板相对光标的偏移。
const OFFSET: f32 = 24.;
/// 面板下方的标签行高（色板 + HEX 文本）。
const LABEL_H: f32 = 26.;

fn grid_span(grid: usize) -> f32 {
    grid as f32 * CELL + (grid as f32 - 1.) * GAP
}

fn panel_w(grid: usize) -> f32 {
    grid_span(grid) + PAD * 2.
}

fn panel_h(grid: usize) -> f32 {
    PAD + grid_span(grid) + GAP + LABEL_H + PAD
}

fn color_u32(c: Rgba<u8>) -> u32 {
    ((c[0] as u32) << 16) | ((c[1] as u32) << 8) | c[2] as u32
}

fn hex_of(c: Rgba<u8>) -> String {
    format!("#{:02X}{:02X}{:02X}", c[0], c[1], c[2])
}

/// AppKit 的 NSPoint（= CGPoint）：msg_send 按值传结构体需要本地 `#[repr(C)]`
/// 同布局类型 + `Encode`（CGSize 同款做法）。
#[repr(C)]
#[derive(Clone, Copy)]
struct NSPoint {
    x: f64,
    y: f64,
}
unsafe impl objc2::encode::Encode for NSPoint {
    const ENCODING: objc2::encode::Encoding =
        objc2::encode::Encoding::Struct("CGPoint", &[f64::ENCODING; 2]);
}

/// 把窗口改成 borderless 并覆盖整块屏幕（含菜单栏）。
///
/// Titled 窗口的 `constrainFrameRect` 会把 frame 压在菜单栏下方，borderless
/// 不受约束；层级抬到 `NSStatusWindowLevel = 25` 盖住菜单栏（`NSMainMenuWindowLevel
/// = 24`）。坐标转换：CG 全局坐标（xcap `Monitor::x()/y()`，主屏左上原点，y 向下）
/// → AppKit 全局坐标（主屏左下原点，y 向上），顶左点 y = 主屏高 - cg_y。
#[cfg(target_os = "macos")]
fn make_screen_covering(window: &Window, monitor_origin: (f64, f64)) {
    use objc2::msg_send;
    use objc2::runtime::AnyObject;
    use raw_window_handle::RawWindowHandle;

    let primary_h = core_graphics::display::CGDisplay::main()
        .bounds()
        .size
        .height;
    let top_left = NSPoint {
        x: monitor_origin.0,
        y: primary_h - monitor_origin.1,
    };

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
        let _: () = msg_send![ns_window, setFrameTopLeftPoint: top_left];
        let _: () = msg_send![ns_window, setLevel: 25i64];
        let _: () = msg_send![ns_window, setHasShadow: false];
        let _: () = msg_send![ns_window, setAcceptsMouseMovedEvents: true];
    }
}

pub(crate) struct PickerOverlay {
    snapshot: ScreenSnapshot,
    /// 整屏快照转成的背景位图：取色画面整体定格，dock / 菜单栏以静态图像
    /// 可见但零交互（真 dock 被 setPresentationOptions 隐藏，浮窗层级也盖住
    /// 桌面）；放大镜采样与背景用同一份快照，天然对齐。
    background: Arc<RenderImage>,
    /// 光标在窗口内的逻辑坐标；面板与放大网格都围绕它。鼠标不在本屏时为
    /// None（不渲染面板，纯覆盖层）。
    cursor: Option<Point<Pixels>>,
    /// 放大网格边长（奇数），滚轮调节。
    grid: usize,
    focus_handle: FocusHandle,
}

impl PickerOverlay {
    fn new(snapshot: ScreenSnapshot, window: &mut Window, cx: &mut Context<Self>) -> Self {
        // 开窗瞬间即用截屏时的鼠标位置摆好面板，用户不动鼠标也能看到放大镜。
        let cursor = snapshot
            .mouse_local
            .map(|(x, y)| point(px(x as f32), px(y as f32)));
        // 背景 clone 一份进 RenderImage（Retina 全屏约几十 MB，取色是短生
        // 命周期操作），原份留在 snapshot 里供放大镜/拾色采样。gpui 的
        // atlas 纹理是 BGRA8，内置解码路径都在进 GPU 前做过 RGBA→BGRA 交换
        // （platform.rs decode_static_image_from_decoder），xcap 的 RGBA
        // 数据必须同样交换，否则背景红蓝互换。
        let mut bgra = snapshot.image.clone();
        for pixel in bgra.as_chunks_mut::<4>().0 {
            pixel.swap(0, 2);
        }
        let background = Arc::new(RenderImage::new(vec![image::Frame::new(bgra)]));
        make_screen_covering(window, snapshot.origin);
        let focus_handle = cx.focus_handle();
        focus_handle.focus(window, cx);
        Self {
            snapshot,
            background,
            cursor,
            grid: GRID,
            focus_handle,
        }
    }

    /// 光标下的像素：逻辑坐标 × 缩放 = 物理索引，越界钳制到边缘。
    fn pixel_at(&self, position: Point<Pixels>) -> Rgba<u8> {
        let (img_w, img_h) = self.snapshot.image.dimensions();
        let scale = self.snapshot.scale as f64;
        let x = (position.x.to_f64().max(0.) * scale) as u32;
        let y = (position.y.to_f64().max(0.) * scale) as u32;
        *self
            .snapshot
            .image
            .get_pixel(x.min(img_w - 1), y.min(img_h - 1))
    }

    fn pick(&self, position: Point<Pixels>, window: &mut Window, cx: &mut App) {
        let hex = hex_of(self.pixel_at(position));
        if copy_text(&hex).is_ok() {
            notify("屏幕取色", &format!("{hex} 已复制到剪切板"));
        } else {
            notify("屏幕取色", "复制到剪切板失败");
        }
        close_all_group(Some(window.window_handle()), cx);
        window.remove_window();
    }
}

impl Focusable for PickerOverlay {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for PickerOverlay {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let overlay = div()
            .size_full()
            .relative()
            .occlude()
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(|_, event: &KeyDownEvent, window, cx| {
                if event.keystroke.key == "escape" {
                    close_all_group(Some(window.window_handle()), cx);
                    window.remove_window();
                }
            }))
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| {
                this.cursor = Some(event.position);
                cx.notify();
            }))
            .on_mouse_exit(cx.listener(|this, _, _, cx| {
                // 鼠标切到了别的显示器：收起本屏的放大镜（那边屏幕的浮窗接管）。
                if this.cursor.take().is_some() {
                    cx.notify();
                }
            }))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|_, _, window, cx| {
                    close_all_group(Some(window.window_handle()), cx);
                    window.remove_window();
                }),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, window, cx| {
                    this.pick(event.position, window, cx);
                }),
            )
            .on_scroll_wheel(cx.listener(|this, event: &ScrollWheelEvent, _, cx| {
                let dy = match event.delta {
                    ScrollDelta::Lines(p) => p.y as f64,
                    ScrollDelta::Pixels(p) => p.y.to_f64(),
                };
                if dy == 0. {
                    return;
                }
                // 步进 2 保持奇数：上滚扩视野，下滚缩小，[15, 51] 封顶封底。
                let next = this.grid as isize + if dy > 0. { 2 } else { -2 };
                let next = next.clamp(GRID_MIN as isize, GRID_MAX as isize) as usize;
                if next != this.grid {
                    this.grid = next;
                    cx.notify();
                }
            }))
            // 最底层铺整屏定格快照：画面与放大镜采样同源；dock/菜单栏在画面
            // 里可见，但下面是真的已隐藏/被本浮窗盖住，点击只会落到本浮窗。
            .child(
                img(self.background.clone())
                    .size_full()
                    .object_fit(ObjectFit::Fill),
            );

        // 鼠标不在本屏（多屏时只有一块屏有光标）：只渲染透明覆盖层承接
        // 事件，放大镜由光标所在屏的浮窗负责。
        let Some(cursor) = self.cursor else {
            return overlay;
        };

        let (img_w, img_h) = self.snapshot.image.dimensions();
        let scale = self.snapshot.scale as f64;
        let center_px = self.pixel_at(cursor);
        let hex = hex_of(center_px);
        let cx_i = (cursor.x.to_f64().max(0.) * scale) as i64;
        let cy_i = (cursor.y.to_f64().max(0.) * scale) as i64;

        // 面板摆在光标右下；贴近视口右/下边缘时翻到左上。
        let view = window.viewport_size();
        let (pw, ph) = (panel_w(self.grid) as f64, panel_h(self.grid) as f64);
        let mut x = cursor.x.to_f64() + OFFSET as f64;
        if x + pw > view.width.to_f64() {
            x = cursor.x.to_f64() - OFFSET as f64 - pw;
        }
        let mut y = cursor.y.to_f64() + OFFSET as f64;
        if y + ph > view.height.to_f64() {
            y = cursor.y.to_f64() - OFFSET as f64 - ph;
        }

        // 放大网格：第 (row, col) 格显示快照里中心像素周围 (±half, ±half)
        // 的那个物理像素；越出画面时钳到边缘色。
        let half = (self.grid / 2) as i64;
        let rows = (0..self.grid).map(|row| {
            let sy = (cy_i + row as i64 - half).clamp(0, img_h as i64 - 1) as u32;
            h_flex().gap(px(GAP)).children((0..self.grid).map(|col| {
                let sx = (cx_i + col as i64 - half).clamp(0, img_w as i64 - 1) as u32;
                let color = *self.snapshot.image.get_pixel(sx, sy);
                let is_center = col == self.grid / 2 && row == self.grid / 2;
                div()
                    .flex_none()
                    .size(px(CELL))
                    .bg(rgb(color_u32(color)))
                    .when(is_center, |d| {
                        d.border_2().border_color(rgb(0xFFFFFF)).shadow_sm()
                    })
            }))
        });

        overlay.child(
            div()
                .absolute()
                .left(px(x as f32))
                .top(px(y as f32))
                .w(px(panel_w(self.grid)))
                .bg(cx.theme().popover)
                .border_1()
                .border_color(cx.theme().border)
                .rounded_lg()
                .shadow_lg()
                .p(px(PAD))
                .child(v_flex().gap(px(GAP)).children(rows))
                .child(
                    h_flex()
                        .mt(px(GAP))
                        .h(px(LABEL_H))
                        .items_center()
                        .gap(px(6.))
                        .child(
                            div()
                                .flex_none()
                                .size(px(14.))
                                .bg(rgb(color_u32(center_px)))
                                .border_1()
                                .border_color(cx.theme().border),
                        )
                        .child(
                            div()
                                .text_sm()
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(cx.theme().popover_foreground)
                                .child(hex),
                        )
                        // 显示坐标
                        .child(
                            div()
                                .text_sm()
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(cx.theme().popover_foreground)
                                .child(format!("({} x {})", cx_i, cy_i)),
                        ),
                ),
        )
    }
}

/// 关闭整组取色浮窗并恢复系统 UI（Dock/菜单栏）。
///
/// `self_handle` 是发起调用的浮窗自己：它此刻正被事件回调借用着（gpui 实体
/// 重入 update 会 panic），由调用方随后自己 `remove_window`，这里只关其余的。
/// 从 `open()` 等实体外上下文调用时传 `None`。
fn close_all_group(self_handle: Option<AnyWindowHandle>, cx: &mut App) {
    let handles = std::mem::take(&mut cx.global_mut::<AppShell>().color_picker);
    for handle in handles {
        if Some(handle) == self_handle {
            continue;
        }
        let _ = handle.update(cx, |_, window, _| window.remove_window());
    }
    set_ui_presentation(false);
}

/// 打开取色浮窗组：每块显示器一个；旧的还开着就先整组关掉（防叠加）。
pub(crate) fn open(cx: &mut App) {
    close_all_group(None, cx);

    let snapshots = match capture::capture_all() {
        Ok(s) => s,
        Err(e) => {
            // 权限缺失的提示已经弹过系统横幅，这里只记 console。
            cx.global::<AppShell>()
                .console_channel
                .warn(format!("屏幕取色：{e}"));
            return;
        }
    };

    for snapshot in snapshots {
        let (w, h) = snapshot.logical_size;
        let options = WindowOptions {
            // 覆盖该显示器：origin 相对该屏本地坐标（gpui 会转 AppKit 坐标），
            // size 为该屏逻辑尺寸。
            window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                point(px(0.), px(0.)),
                size(px(w as f32), px(h as f32)),
            ))),
            display_id: snapshot.display_id,
            titlebar: None,
            // 用 Normal（NSWindow）开窗、事后 FFI 转 borderless 提 level——
            // Floating 走 NSPanel 路径，frame/透明/收键都有坑。
            kind: WindowKind::Normal,
            is_resizable: false,
            focus: true,
            window_background: WindowBackgroundAppearance::Transparent,
            ..Default::default()
        };
        // 用框架层 App::open_window 而非 gpui_kit::open_window：后者把内容包进
        // base::Root 并挂上 gpui-component 的 WindowState 插件，其 style 回调给每个
        // 窗口强制垫 theme().background（不透明），透明覆盖窗会被整体垫白。
        // 取色浮窗不依赖 Root 的组件能力（tooltip/toast/dialog 等），直接裸开。
        match cx.open_window(options, |window, cx| {
            cx.new(|cx| PickerOverlay::new(snapshot, window, cx))
        }) {
            Ok(handle) => {
                cx.global_mut::<AppShell>().color_picker.push(handle.into());
            }
            Err(e) => {
                cx.global::<AppShell>()
                    .console_channel
                    .warn(format!("无法打开取色浮窗：{e}"));
            }
        }
    }

    // 全部开完后隐藏 Dock/菜单栏：Dock 的 hover 放大监听的是全局鼠标位置，
    // 无法从外部屏蔽，隐藏是最可靠的方式（快照画面里仍有 dock 静态图像，
    // 视觉上 dock 还在但零交互）。结束时在 close_all_group 里恢复。
    if !cx.global::<AppShell>().color_picker.is_empty() {
        set_ui_presentation(true);
    }
}

/// 隐藏 / 恢复系统 UI（Dock + 菜单栏），取色期间零干扰。
///
/// `NSApplication.setPresentationOptions` 只对本 App 处于激活态时生效——
/// 取色浮窗会抢焦点，正好满足；浮窗关闭后这里传 false 复原。
#[cfg(target_os = "macos")]
fn set_ui_presentation(hidden: bool) {
    use objc2::msg_send;
    use objc2::runtime::{AnyClass, AnyObject};

    let Some(app_class) = AnyClass::get(c"NSApplication") else {
        return;
    };
    unsafe {
        let app: *mut AnyObject = msg_send![app_class, sharedApplication];
        if app.is_null() {
            return;
        }
        // NSApplicationPresentationHideDock = 1 << 1，HideMenuBar = 1 << 3。
        let options: usize = if hidden { (1 << 1) | (1 << 3) } else { 0 };
        let _: () = msg_send![app, setPresentationOptions: options];
    }
}

#[cfg(not(target_os = "macos"))]
fn set_ui_presentation(_hidden: bool) {}
