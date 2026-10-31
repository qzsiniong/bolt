//! 独立 Console 窗口的开窗逻辑（单例）。

use gpui_kit::{App, AppContext as _};

use super::ConsoleState;
use super::panel::ConsolePanel;
use crate::app::shell::AppShell;
use crate::ui::window::{WindowSpec, open_singleton};

/// 打开（或聚焦）Console 窗口。
pub(crate) fn open_console(cx: &mut App) {
    let log = cx.global::<AppShell>().console_channel.history();
    // Console 默认置顶：调试时的常驻仪表盘，被编辑器/浏览器盖住就失去了意义。
    // 层级在 `open_singleton` 的 build 闭包内、视图创建之前设置。
    let spec = WindowSpec::new("Console")
        .size(900., 560.)
        .min_size(560., 360.)
        .pinned(true);

    let existing = cx.global_mut::<ConsoleState>().window.take();
    match open_singleton(&spec, cx, existing, |window, cx| {
        cx.new(|cx| ConsolePanel::new_windowed(window, log, cx))
    }) {
        Ok(handle) => {
            cx.global_mut::<ConsoleState>().window = Some(handle);
        }
        Err(error) => {
            let console = cx.global::<AppShell>().console_channel.clone();
            console.error(format!("无法打开 Console 窗口：{error}"));
        }
    }
}
