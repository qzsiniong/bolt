//! Console 模块：把应用的共享输出缓冲变成一个独立窗口，并在 Launcher 里注册
//! 一条「Console」命令。
//!
//! 输出本身属于应用层（`crate::console_channel`）：脚本运行时、扩展宿主、UI 扩展
//! 的 tracing、启动期错误都写同一个 [`ConsoleLog`]，视图（脚本管理器的 dock 面板
//! 与这个独立窗口）各自持游标读增量。

pub(crate) mod panel;
pub(crate) mod window;

use std::sync::Arc;

use gpui_kit::assets::IconName;
use gpui_kit::{AnyWindowHandle, App, Global};

use crate::app::launcher::{LauncherItem, Module};

/// Console 模块状态：独立窗口的单例句柄。
pub(crate) struct ConsoleState {
    pub(crate) window: Option<AnyWindowHandle>,
}

impl Global for ConsoleState {}

/// Console 模块：一条命令，开独立窗口。
pub(crate) struct ConsoleModule;

impl Module for ConsoleModule {
    fn name(&self) -> &'static str {
        "console"
    }

    fn collect_items(&self, _cx: &App) -> Vec<LauncherItem> {
        vec![item()]
    }
}

/// 那条「Console」命令。纯函数，方便单测（不需要 `App`）。
fn item() -> LauncherItem {
    LauncherItem {
        id: "console.open".to_string(),
        title: "Console".to_string(),
        subtitle: Some("查看日志、脚本输出与启动报错".to_string()),
        icon: IconName::Terminal.into(),
        group: "内置".to_string(),
        keywords: vec![
            "console".to_string(),
            "log".to_string(),
            "logs".to_string(),
            "日志".to_string(),
            "输出".to_string(),
        ],
        on_confirm: Arc::new(window::open_console),
        actions: Vec::new(),
        paste: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_console_item_points_at_the_standalone_window() {
        let item = item();
        assert_eq!(item.id, "console.open");
        assert_eq!(item.title, "Console");
        assert_eq!(item.group, "内置");
        // 中英文都能搜到：中文输入法和英文习惯各一套。
        for keyword in ["console", "log", "日志"] {
            assert!(
                item.keywords.iter().any(|k| k == keyword),
                "missing keyword {keyword}: {:?}",
                item.keywords
            );
        }
    }
}
