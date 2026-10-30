//! 「扩展管理」窗口的开窗逻辑（单例）。

use gpui_kit::component::TitleBar;
use gpui_kit::{
    App, AppContext as _, Bounds, TitlebarOptions, WindowBounds, WindowKind, WindowOptions, px,
    size,
};

use super::view::PluginManagerView;
use crate::app::shell::AppShell;
use crate::modules::extensions::state::ExtensionsState;

/// 打开（或聚焦）扩展管理窗口。
pub(crate) fn open_manager(cx: &mut App) {
    // 从 launcher 动作里开窗时 launcher 正关着，不抢一次前台会被压到别的 App 后面。
    cx.activate(true);

    let existing = cx.global_mut::<ExtensionsState>().manager.take();
    if let Some(handle) = existing
        && handle
            .update(cx, |_, window, _| window.activate_window())
            .is_ok()
    {
        cx.global_mut::<ExtensionsState>().manager = Some(handle);
        return;
    }

    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
            None,
            size(px(960.), px(640.)),
            cx,
        ))),
        kind: WindowKind::Normal,
        window_min_size: Some(size(px(720.), px(460.))),
        titlebar: Some(TitlebarOptions {
            title: Some("扩展管理".into()),
            ..TitleBar::title_bar_options()
        }),
        is_resizable: true,
        focus: true,
        ..TitleBar::window_options()
    };

    let opened = gpui_kit::open_window(options, cx, |_window, cx| {
        cx.new(|cx| PluginManagerView::new(cx))
    });
    match opened {
        Ok((handle, _view)) => {
            cx.global_mut::<ExtensionsState>().manager = Some(handle);
        }
        Err(error) => {
            let console = cx.global::<AppShell>().console_channel.clone();
            console.error(format!("无法打开扩展管理窗口：{error}"));
        }
    }
}
