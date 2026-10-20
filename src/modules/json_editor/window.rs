//! JSON 编辑器窗口的开窗逻辑（单例）。

use gpui_kit::component::TitleBar;
use gpui_kit::{
    App, AppContext as _, Bounds, TitlebarOptions, WindowBounds, WindowKind, WindowOptions, px,
    size,
};

use super::panel::JsonEditorPanel;
use super::state::JsonEditorState;
use crate::app::shell::AppShell;

/// Open the native JSON editor window as a singleton — focus the existing
/// handle if it's still alive, otherwise activate the app and create a fresh
/// one. Mirrors the script manager's `open_manager`.
pub(crate) fn open_json_editor(cx: &mut App) {
    cx.activate(true);

    let existing = cx.global_mut::<JsonEditorState>().window.take();
    if let Some(handle) = existing {
        let alive = handle
            .update(cx, |_, window, _| window.activate_window())
            .is_ok();
        if alive {
            cx.global_mut::<JsonEditorState>().window = Some(handle);
            return;
        }
    }

    let console = cx.global::<AppShell>().console_channel.clone();
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
            None,
            size(px(880.), px(720.)),
            cx,
        ))),
        kind: WindowKind::Normal,
        window_min_size: Some(size(px(520.), px(380.))),
        titlebar: Some(TitlebarOptions {
            title: Some("JSON 编辑器".into()),
            ..TitleBar::title_bar_options()
        }),
        is_resizable: true,
        focus: true,
        // 以 `TitleBar::window_options()` 为基底：标题栏由视图自绘（含右上角
        // 置顶图钉），拖动/双击交给 `TitleBar` 自己处理，AppKit 不再把它当
        // 系统拖动区。
        ..TitleBar::window_options()
    };

    let opened = gpui_kit::open_window(options, cx, |window, cx| {
        cx.new(|cx| JsonEditorPanel::new(window, cx))
    });
    match opened {
        Ok((handle, _panel)) => {
            cx.global_mut::<JsonEditorState>().window = Some(handle);
        }
        Err(e) => {
            console.error(format!("无法打开 JSON 编辑器窗口：{e}"));
        }
    }
}
