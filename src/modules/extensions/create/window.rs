//! 「新建扩展」窗口的开窗逻辑（单例）。

use gpui_kit::{App, AppContext as _};

use super::form::CreateExtensionView;
use crate::app::shell::AppShell;
use crate::modules::extensions::state::ExtensionsState;
use crate::ui::window::{WindowSpec, open_singleton};

pub(crate) fn open_create(cx: &mut App) {
    let spec = WindowSpec::new("新建扩展")
        .size(640., 560.)
        .min_size(560., 480.);

    let existing = cx.global_mut::<ExtensionsState>().create.take();
    match open_singleton(&spec, cx, existing, |window, cx| {
        cx.new(|cx| CreateExtensionView::new(window, cx))
    }) {
        Ok(handle) => {
            cx.global_mut::<ExtensionsState>().create = Some(handle);
        }
        Err(error) => {
            let console = cx.global::<AppShell>().console_channel.clone();
            console.error(format!("无法打开新建扩展窗口：{error}"));
        }
    }
}
