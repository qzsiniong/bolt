//! Native JSON editor module: a strict-JSON editor window with syntax
//! highlighting and folding, plus a single launcher item that opens it.

pub(crate) mod panel;
pub(crate) mod parsers;
pub(crate) mod state;
pub(crate) mod window;

use std::sync::Arc;

use gpui_kit::App;
use gpui_kit::assets::IconName;

use crate::app::launcher::{LauncherItem, Module};

/// JSON 编辑器模块：只贡献一条「JSON 编辑器」内置命令。
pub(crate) struct JsonEditorModule;

impl Module for JsonEditorModule {
    fn name(&self) -> &'static str {
        "json_editor"
    }

    fn collect_items(&self, _cx: &App) -> Vec<LauncherItem> {
        vec![open_item()]
    }
}

/// 纯函数：恒定的单条「JSON 编辑器」命令，与 App 上下文解耦方便单测。
fn open_item() -> LauncherItem {
    LauncherItem {
        id: "json_editor.open".to_string(),
        title: "JSON 编辑器".to_string(),
        subtitle: Some("原生 JSON 编辑器（严格 JSON，语法高亮 + 折叠）".to_string()),
        icon: IconName::Braces,
        group: "内置".to_string(),
        on_confirm: Arc::new(window::open_json_editor),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contributes_a_single_builtin_item() {
        let item = open_item();
        assert_eq!(item.id, "json_editor.open");
        assert_eq!(item.group, "内置");
        assert_eq!(item.title, "JSON 编辑器");
    }
}
