//! 便签窗口的开窗逻辑 + 剪切板 pin 入口。

use gpui_kit::{App, AppContext as _, Bounds, WindowBounds, WindowKind, WindowOptions, px};

use super::clipboard::{self, PinContent};
use super::panel::PinWindow;
use crate::app::shell::AppShell;
use crate::ui::window_pin::set_window_level;

/// 在剪切板里读到有效内容时把它 pin 到桌面上。这是 Launcher 命令与
/// 全局快捷键最终都会走到的入口。
pub(crate) fn open_clipboard_pin(cx: &mut App) {
    let console = cx.global::<AppShell>().console_channel.clone();
    let Some(content) = clipboard::read_clipboard(&console) else {
        console.warn("剪切板里没有可 pin 的内容（需要图片或文本）");
        return;
    };
    open_pin(cx, content);
}

/// 内部：把一个已读到的 [`PinContent`] 开成便签窗口。
pub(crate) fn open_pin(cx: &mut App, content: PinContent) {
    // 与便签里 `img` 的显式尺寸共用同一份计算：窗口尺寸若和图片元素不一致，
    // 图片会按元素尺寸（而不是窗口）铺，出现大片空白或裁切。
    let (w, h) = content.display_size();
    // 和 launcher 一致：显示在**鼠标所在那块屏幕**的正中（display_id 与
    // bounds 必须同时给，mac 端要靠 display_id 选 NSScreen）。
    let display_id = crate::app::windows::display_under_cursor();
    let bounds = Bounds::centered(display_id, gpui_kit::size(px(w), px(h)), cx);

    // 记一行尺寸：这张便签到底 pin 了什么，从 Console 就能看见，不必再靠截图猜。
    if let PinContent::Image {
        bytes,
        width,
        height,
        ..
    } = &content
    {
        cx.global::<AppShell>().console_channel.info(format!(
            "pin 图片：{width}×{height}，{} KB，显示 {w:.0}×{h:.0}",
            bytes.len() / 1024
        ));
    }

    let opened = gpui_kit::open_window(
        WindowOptions {
            // 无边框：titlebar=None 让 GPUI 直接开一个内容窗口（没有系统标题栏）。
            titlebar: None,
            kind: WindowKind::Normal,
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            display_id,
            is_resizable: false,
            show: true,
            focus: true,
            ..WindowOptions::default()
        },
        cx,
        |window, cx| cx.new(|cx| PinWindow::new(content, window, cx)),
    );
    match opened {
        Ok((handle, _view)) => {
            // 立刻抬到 Floating 级别（NSWindowFloatingLevel = 3），便签
            // 就悬浮在所有普通应用窗口之上。复用已有的 set_window_level。
            let _ = handle.update(cx, |_, window, _| set_window_level(window, true));
        }
        Err(e) => {
            cx.global::<AppShell>()
                .console_channel
                .warn(format!("无法打开 pin 窗口：{e}"));
        }
    }
}
