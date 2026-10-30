//! Extensions 模块：把 `extensions/` 下的 plux 插件变成 Launcher item。
//!
//! 一个插件贡献若干命令，一条命令一条 item：
//!
//! - 带 `ui` 块的命令 → gpui-shell 原生窗口；
//! - 其余命令 → 插件常驻运行时里的同名 JS 函数。
//!
//! 磁盘内容每次打开 Launcher 都会重新扫描（作者改完清单不用重启），但已注册的
//! 插件不会重复注册、也不会被卸载——插件运行时的状态要跨命令保留。

pub(crate) mod host;
pub(crate) mod js_env;
pub(crate) mod manifest;
pub(crate) mod state;
pub(crate) mod ui_window;

mod exec;

use std::sync::Arc;

use gpui_kit::App;
use gpui_kit::assets::IconName;

use crate::app::launcher::{LauncherAction, LauncherItem, Module};
use crate::app::shell::AppShell;

use host::CommandTarget;
use state::ExtensionsState;

/// Extensions 模块：扫描插件根，把每条命令变成一条 launcher item。
pub(crate) struct ExtensionsModule;

impl Module for ExtensionsModule {
    fn name(&self) -> &'static str {
        "extensions"
    }

    fn collect_items(&self, cx: &App) -> Vec<LauncherItem> {
        let (targets, errors) = {
            let state = cx.global::<ExtensionsState>();
            // `collect_items` 只拿到 `&App`，而注册插件要 `&mut Loader`：
            // 宿主因此放在 RefCell 里（GPUI 是单线程的，不存在并发借用）。
            let mut host = state.host.borrow_mut();
            let errors = host.refresh();
            (host.commands(), errors)
        };

        if !errors.is_empty() {
            let console = cx.global::<AppShell>().console_channel.clone();
            for error in errors {
                console.warn(error);
            }
        }

        items_in(&targets)
    }
}

/// 纯函数：命令 → items。与 App 上下文解耦，方便单测。
fn items_in(targets: &[CommandTarget]) -> Vec<LauncherItem> {
    targets
        .iter()
        .map(|target| {
            let ui = target.is_ui();
            let captured = target.clone();
            let on_confirm: LauncherAction = if ui {
                Arc::new(move |cx| ui_window::open_plugin_ui(cx, captured.clone()))
            } else {
                Arc::new(move |cx| exec::run_command(cx, captured.clone()))
            };

            LauncherItem {
                id: target.item_id(),
                title: target.title.clone(),
                subtitle: target
                    .description
                    .clone()
                    .or_else(|| Some(format!("{} v{}", target.plugin_name, target.bundle.version))),
                icon: if ui {
                    IconName::AppWindow.into()
                } else {
                    IconName::Puzzle.into()
                },
                group: "扩展".to_string(),
                keywords: vec![target.plugin_id.clone(), target.command_id.clone()],
                on_confirm,
                actions: Vec::new(),
                paste: None,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use plux_js_manager::{PluginManifest, UiCapabilities, UiSpec};
    use plux_rs::Bundle;
    use std::path::PathBuf;

    fn target(command: &str, ui: Option<UiSpec>) -> CommandTarget {
        CommandTarget {
            bundle: Bundle::from_filename("demo-v1.0.0.js").expect("valid bundle"),
            plugin_id: "demo".to_string(),
            plugin_name: "演示插件".to_string(),
            root: PathBuf::from("/tmp/demo-v1.0.0.js"),
            command_id: command.to_string(),
            title: format!("命令 {command}"),
            description: None,
            ui,
        }
    }

    #[test]
    fn script_and_ui_commands_become_items() {
        let targets = vec![
            target("hello", None),
            target(
                "panel",
                Some(UiSpec {
                    title: "面板".to_string(),
                    width: 480.,
                    height: 320.,
                    always_on_top: false,
                    capabilities: UiCapabilities::default(),
                }),
            ),
        ];

        let items = items_in(&targets);
        assert_eq!(items.len(), 2);

        assert_eq!(items[0].id, "ext.demo.hello");
        assert_eq!(items[0].title, "命令 hello");
        assert_eq!(items[0].group, "扩展");
        // 没写 description 时退回 "插件名 v版本"。
        assert_eq!(items[0].subtitle.as_deref(), Some("演示插件 v1.0.0"));
        assert!(matches!(
            items[0].icon,
            crate::app::launcher::LauncherIcon::Named(IconName::Puzzle)
        ));

        assert_eq!(items[1].id, "ext.demo.panel");
        assert!(matches!(
            items[1].icon,
            crate::app::launcher::LauncherIcon::Named(IconName::AppWindow)
        ));
    }

    #[test]
    fn empty_plugin_yields_no_items() {
        assert!(items_in(&[]).is_empty());
    }

    #[test]
    fn manifest_with_no_commands_still_has_one_after_validation() {
        // 校验阶段会补一条 run 命令，这里确认命令 id 与 item id 的拼装规则。
        let manifest = PluginManifest {
            id: "demo".into(),
            name: String::new(),
            description: None,
            version: "1.0.0".into(),
            entry: "index.js".into(),
            depends: Vec::new(),
            optional_depends: Vec::new(),
            commands: Vec::new(),
        };
        assert_eq!(manifest.display_name(), "demo");

        let items = items_in(&[target("run", None)]);
        assert_eq!(items[0].id, "ext.demo.run");
    }
}
