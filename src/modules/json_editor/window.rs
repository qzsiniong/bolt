//! JSON 编辑器窗口的开窗逻辑（单例）。

use gpui_kit::{App, AppContext as _};

use super::panel::JsonEditorPanel;
use super::state::JsonEditorState;
use crate::app::shell::AppShell;
use crate::ui::window::{WindowSpec, open_singleton};

/// 以单例方式打开原生 JSON 编辑器窗口——已有句柄还活着就激活它，否则新建。
pub(crate) fn open_json_editor(cx: &mut App) {
    open_json_editor_with(cx, String::new())
}

/// 同 [`open_json_editor`]，但以给定内容开窗（粘贴 JSON 时的入口）。已存在
/// 的编辑器窗口被激活时**不**写入新内容（避免覆盖用户正在编辑的东西），
/// 只有新开窗口才带初始内容——这正是 [`open_singleton`] 的「已活即激活并
/// 返回、不跑 build」语义。
pub(crate) fn open_json_editor_with(cx: &mut App, content: String) {
    let spec = WindowSpec::new("JSON 编辑器")
        .size(880., 720.)
        .min_size(520., 380.)
        .pinned(true);

    let existing = cx.global_mut::<JsonEditorState>().window.take();
    let console = cx.global::<AppShell>().console_channel.clone();
    match open_singleton(&spec, cx, existing, |window, cx| {
        cx.new(|cx| JsonEditorPanel::new(window, cx, content))
    }) {
        Ok(handle) => {
            cx.global_mut::<JsonEditorState>().window = Some(handle);
        }
        Err(e) => {
            console.error(format!("无法打开 JSON 编辑器窗口：{e}"));
        }
    }
}
