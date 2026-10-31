//! 跨模块的 UI 通用件：宿主交互浮层、窗口图钉控件，以及编辑器面板绑定的
//! 纯文本编辑操作。按功能划分的视图（脚本管理、Launcher、JSON 编辑器）在
//! `src/modules/<feature>/` 下。

pub(crate) mod clipboard;
pub(crate) mod editor_ops;
pub(crate) mod interaction_window;
#[cfg(target_os = "macos")]
pub(crate) mod mac_ffi;
pub(crate) mod notify_window;
pub(crate) mod window;
