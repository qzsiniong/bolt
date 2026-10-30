//! 独立 Console 窗口的开窗逻辑（单例）。

use gpui_kit::component::TitleBar;
use gpui_kit::{
    App, AppContext as _, Bounds, TitlebarOptions, WindowBounds, WindowKind, WindowOptions, px,
    size,
};

use super::ConsoleState;
use super::panel::ConsolePanel;
use crate::app::shell::AppShell;

/// 打开（或聚焦）Console 窗口。
pub(crate) fn open_console(cx: &mut App) {
    // 从 launcher 动作里开窗时 launcher 正关着，不抢一次前台会被压到别的 App 后面。
    cx.activate(true);

    let existing = cx.global_mut::<ConsoleState>().window.take();
    if let Some(handle) = existing
        && handle
            .update(cx, |_, window, _| window.activate_window())
            .is_ok()
    {
        cx.global_mut::<ConsoleState>().window = Some(handle);
        return;
    }

    let log = cx.global::<AppShell>().console_channel.history();
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
            None,
            size(px(900.), px(560.)),
            cx,
        ))),
        kind: WindowKind::Normal,
        window_min_size: Some(size(px(560.), px(360.))),
        titlebar: Some(TitlebarOptions {
            title: Some("Console".into()),
            ..TitleBar::title_bar_options()
        }),
        is_resizable: true,
        focus: true,
        ..TitleBar::window_options()
    };
    // 默认置顶不在这里写：`WindowKind::Floating` 与 `Normal` 只差窗口层级，
    // 而层级由 `ConsolePanel` 建视图时调 `set_window_level(true)` 设定——与标题栏
    // 图钉共用同一条路径，避免「开窗设一次、图钉再设一次」的两处真相。

    let opened = gpui_kit::open_window(options, cx, move |window, cx| {
        cx.new(|cx| ConsolePanel::new_windowed(window, log, cx))
    });
    match opened {
        Ok((handle, _view)) => {
            cx.global_mut::<ConsoleState>().window = Some(handle);
        }
        Err(error) => {
            let console = cx.global::<AppShell>().console_channel.clone();
            console.error(format!("无法打开 Console 窗口：{error}"));
        }
    }
}
