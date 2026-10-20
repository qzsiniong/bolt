//! Scripts 模块的应用级状态：脚本管理器窗口单例。

use gpui_kit::{AnyWindowHandle, Global, WeakEntity};

use super::workspace::Workspace;

/// Scripts 模块状态，随 app 生命周期存活的 Global。
pub(crate) struct ScriptsState {
    /// 脚本管理器窗口。workspace 只弱持有：窗口关闭必须把它（及其 console
    /// drain）释放，console 输出在 channel 里缓冲到下一个管理器窗口打开。
    pub(crate) manager: Option<(AnyWindowHandle, WeakEntity<Workspace>)>,
}

impl Global for ScriptsState {}
