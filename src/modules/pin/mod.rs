//! Pin 模块：从剪切板创建桌面悬浮便签（图片 / 文本）。
//!
//! 接入 Launcher 提供一条 "Pin 剪切板" 命令；全局快捷键 `Cmd+Shift+T`
//! 也会触发同一路径。窗口为无边框 Floating，便签内容支持拖拽 / 滚轮缩放 /
//! 双击关闭 / 右键自绘操作菜单（复制 / 保存 / 置顶切换 / 关闭）。

pub(crate) mod clipboard;
pub(crate) mod panel;
pub(crate) mod window;

use std::sync::Arc;

use gpui_kit::App;
use gpui_kit::assets::IconName;

use crate::app::launcher::{LauncherItem, Module};

/// Pin 模块：只贡献一条「Pin 剪切板」内置命令。
pub(crate) struct PinModule;

impl Module for PinModule {
    fn name(&self) -> &'static str {
        "pin"
    }

    fn collect_items(&self, _cx: &App) -> Vec<LauncherItem> {
        vec![LauncherItem {
            id: "pin.from-clipboard".to_string(),
            title: "Pin 剪切板".to_string(),
            subtitle: Some("把剪切板内容固定到桌面（图片或文本，悬浮便签）".to_string()),
            icon: IconName::Pin.into(),
            group: "内置".to_string(),
            keywords: Vec::new(),
            on_confirm: Arc::new(window::open_clipboard_pin),
        }]
    }
}
