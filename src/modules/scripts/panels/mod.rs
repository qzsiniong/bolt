//! 管理窗口的 dock 面板。
//!
//! Console 面板不在这里：它是 `modules::console::panel::ConsolePanel`，同一份
//! 组件也被独立 Console 窗口使用。

pub(crate) mod editor;
pub(crate) mod script_list_panel;

pub use editor::EditorPanel;
pub use script_list_panel::ScriptListPanel;
