//! 「新建扩展」窗口的开窗逻辑（单例）。

use gpui_kit::component::TitleBar;
use gpui_kit::{
    App, AppContext as _, Bounds, TitlebarOptions, WindowBounds, WindowKind, WindowOptions, px,
    size,
};

use super::form::CreateExtensionView;
use crate::app::shell::AppShell;
use crate::modules::extensions::state::ExtensionsState;

pub(crate) fn open_create(cx: &mut App) {
    cx.activate(true);

    let existing = cx.global_mut::<ExtensionsState>().create.take();
    if let Some(handle) = existing
        && handle
            .update(cx, |_, window, _| window.activate_window())
            .is_ok()
    {
        cx.global_mut::<ExtensionsState>().create = Some(handle);
        return;
    }

    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
            None,
            size(px(640.), px(560.)),
            cx,
        ))),
        kind: WindowKind::Normal,
        window_min_size: Some(size(px(560.), px(480.))),
        titlebar: Some(TitlebarOptions {
            title: Some("新建扩展".into()),
            ..TitleBar::title_bar_options()
        }),
        is_resizable: true,
        focus: true,
        ..TitleBar::window_options()
    };

    let opened = gpui_kit::open_window(options, cx, |window, cx| {
        cx.new(|cx| CreateExtensionView::new(window, cx))
    });
    match opened {
        Ok((handle, _view)) => {
            cx.global_mut::<ExtensionsState>().create = Some(handle);
        }
        Err(error) => {
            let console = cx.global::<AppShell>().console_channel.clone();
            console.error(format!("无法打开新建扩展窗口：{error}"));
        }
    }
}
