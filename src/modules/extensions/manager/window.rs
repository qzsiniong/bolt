//! 「扩展管理」窗口的开窗逻辑（单例）。

use gpui_kit::{App, AppContext as _};

use super::view::PluginManagerView;
use crate::app::shell::AppShell;
use crate::modules::extensions::state::ExtensionsState;
use crate::ui::window::{WindowSpec, open_singleton};

/// 打开（或聚焦）扩展管理窗口。
pub(crate) fn open_manager(cx: &mut App) {
    let spec = WindowSpec::new("扩展管理")
        .size(960., 640.)
        .min_size(720., 460.);

    let existing = cx.global_mut::<ExtensionsState>().manager.take();
    match open_singleton(&spec, cx, existing, |_window, cx| {
        cx.new(|cx| PluginManagerView::new(cx))
    }) {
        Ok(handle) => {
            cx.global_mut::<ExtensionsState>().manager = Some(handle);
        }
        Err(error) => {
            let console = cx.global::<AppShell>().console_channel.clone();
            console.error(format!("无法打开扩展管理窗口：{error}"));
        }
    }
}
