//! Scripts 模块的应用级状态：脚本管理器窗口单例。

use gpui_kit::{AnyWindowHandle, Global, WeakEntity};

use super::workspace::Workspace;

/// Scripts 模块状态，随 app 生命周期存活的 Global。
pub(crate) struct ScriptsState {
    /// 脚本管理器窗口。workspace 只弱持有：窗口关闭必须把它（及其 Console
    /// 视图的轮询任务）释放，输出在共享缓冲里留着，等下一次打开窗口时补上。
    pub(crate) manager: Option<(AnyWindowHandle, WeakEntity<Workspace>)>,
}

impl Global for ScriptsState {}
