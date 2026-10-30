//! Extensions 模块的应用级状态：UI 命令窗口单例、扩展资源源、gpui-shell 运行时、
//! plux 插件宿主。

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use gpui_kit::{AnyWindowHandle, Global};

use super::host::ExtensionsHost;
use crate::app::assets::ExtensionAssets;
use crate::host::HostRegistry;

/// Extensions 模块状态，随 app 生命周期存活的 Global。
pub(crate) struct ExtensionsState {
    /// 每个插件目录一个窗口，按插件根键控。句柄失效说明窗口已关（用户关掉 /
    /// 脚本请求退出），下次打开命令时重建。
    pub(crate) ui_windows: HashMap<PathBuf, AnyWindowHandle>,
    /// 复合资源源句柄：开窗前登记插件自己的 `svg()` 相对路径根。
    pub(crate) assets: ExtensionAssets,
    /// UI 命令运行的 gpui-shell 运行时。`None` 表示初始化失败，打开 UI 命令时报错
    /// 而不是拖垮整个应用。
    pub(crate) shell_runtime: Option<Rc<gpui_shell::ShellRuntime>>,
    /// plux 插件宿主。`collect_items` 只拿到 `&App`，而注册插件要 `&mut Loader`，
    /// 故用 `RefCell`（GPUI 单线程，不会并发借用）。
    pub(crate) host: RefCell<ExtensionsHost>,
    /// 「扩展管理」窗口单例。
    pub(crate) manager: Option<AnyWindowHandle>,
    /// 「新建扩展」窗口单例。
    pub(crate) create: Option<AnyWindowHandle>,
    /// 宿主模块注册表：新建扩展时用它写出 `bolt.d.ts`。
    pub(crate) registry: Arc<HostRegistry>,
}

impl Global for ExtensionsState {}
