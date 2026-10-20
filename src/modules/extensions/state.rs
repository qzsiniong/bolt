//! Extensions 模块的应用级状态：UI 扩展窗口单例、扩展资源源、gpui-shell 运行时。

use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;

use gpui_kit::{AnyWindowHandle, Global};

use crate::app::assets::ExtensionAssets;

/// Extensions 模块状态，随 app 生命周期存活的 Global。
pub(crate) struct ExtensionsState {
    /// 每个 UI 扩展一个窗口，按扩展目录键控。句柄失效说明窗口已关
    /// （用户关掉 / 脚本请求退出），下次打开命令时重建。
    pub(crate) ui_windows: HashMap<PathBuf, AnyWindowHandle>,
    /// 复合资源源句柄：开窗前登记扩展自己的 `svg()` 相对路径根。
    pub(crate) assets: ExtensionAssets,
    /// UI 扩展运行的 gpui-shell 运行时。`None` 表示初始化失败，打开
    /// UI 扩展时报错而不是拖垮整个应用。
    pub(crate) shell_runtime: Option<Rc<gpui_shell::ShellRuntime>>,
}

impl Global for ExtensionsState {}
