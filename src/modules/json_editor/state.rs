//! JSON 编辑器模块的应用级状态：窗口单例。

use gpui_kit::{AnyWindowHandle, Global};

/// JSON 编辑器模块状态，随 app 生命周期存活的 Global。
pub(crate) struct JsonEditorState {
    /// JSON 编辑器窗口。句柄失效说明窗口已关，下次打开命令时重建。
    pub(crate) window: Option<AnyWindowHandle>,
}

impl Global for JsonEditorState {}
