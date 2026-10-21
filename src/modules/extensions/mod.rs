//! External extensions module: command bundles living in the extensions
//! directory. Contributes one launcher item per script-extension command and
//! one per UI extension.

pub(crate) mod manifest;
pub(crate) mod state;
pub(crate) mod ui_window;

use std::path::Path;
use std::sync::Arc;

use gpui_kit::App;
use gpui_kit::assets::IconName;

use crate::app::launcher::{LauncherItem, Module};
use crate::app::shell::AppShell;

/// Extensions 模块：扫描扩展目录，把脚本式扩展命令与 UI 扩展各变成一条
/// launcher item。
pub(crate) struct ExtensionsModule;

impl Module for ExtensionsModule {
    fn name(&self) -> &'static str {
        "extensions"
    }

    fn collect_items(&self, cx: &App) -> Vec<LauncherItem> {
        let (items, errors) = items_in(&manifest::active_extensions_dir());
        if !errors.is_empty() {
            let console = cx.global::<AppShell>().console_channel.clone();
            for error in errors {
                console.warn(error);
            }
        }
        items
    }
}

/// 纯函数：扫描 `dir` 产出 items 与错误消息列表。与 App 上下文解耦，
/// 方便单测；`collect_items` 负责把错误打到 console。
fn items_in(dir: &Path) -> (Vec<LauncherItem>, Vec<String>) {
    let mut items = Vec::new();
    let mut errors = Vec::new();

    // 脚本式扩展：清单里的每个命令一条 item，点击直接跑入口脚本。
    let (extensions, ext_errors) = manifest::list_extensions(dir);
    errors.extend(
        ext_errors
            .into_iter()
            .map(|e| format!("扩展加载失败 {}：{}", e.dir.display(), e.message)),
    );
    for ext in extensions {
        for cmd in manifest::commands_of(&ext) {
            let entry = ext.entry.clone();
            items.push(LauncherItem {
                id: format!("ext.{}.{}", ext.manifest.id, cmd.id),
                title: cmd.title,
                subtitle: cmd.subtitle,
                icon: IconName::Puzzle.into(),
                // group: ext.manifest.display_name().to_string(),
                group: "扩展".to_string(),
                on_confirm: Arc::new(move |cx| crate::runtime::run_path(cx, &entry)),
            });
        }
    }

    // UI 扩展：每个一条 item，在它自己的原生窗口里打开。
    let (ui_extensions, ui_errors) = manifest::list_ui_extensions(dir);
    errors.extend(
        ui_errors
            .into_iter()
            .map(|e| format!("扩展加载失败 {}：{}", e.dir.display(), e.message)),
    );
    for ext in ui_extensions {
        let opened = ext.clone();
        items.push(LauncherItem {
            id: format!("ui.{}", ext.id()),
            title: ext.name().to_string(),
            subtitle: Some(format!("gpui-shell · {}", ext.id())),
            icon: IconName::AppWindow.into(),
            // group: ext.name().to_string(),
            group: "扩展".to_string(),
            on_confirm: Arc::new(move |cx| ui_window::open_extension_ui(cx, opened.clone())),
        });
    }

    (items, errors)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TempDir;

    #[test]
    fn empty_dir_yields_no_items_and_no_errors() {
        let tmp = TempDir::new("ext-items-empty");
        let (items, errors) = items_in(&tmp.0);
        assert!(items.is_empty());
        assert!(errors.is_empty());
    }

    #[test]
    fn script_extension_commands_become_items() {
        let tmp = TempDir::new("ext-items-script");
        tmp.write(
            "demo/extension.json",
            r#"{ "id": "demo", "commands": [{ "id": "go", "title": "Go" }] }"#,
        );
        tmp.write("demo/index.js", "");

        let (items, errors) = items_in(&tmp.0);
        assert!(errors.is_empty());
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].id, "ext.demo.go");
        assert_eq!(items[0].title, "Go");
        assert_eq!(items[0].group, "扩展");
    }

    #[test]
    fn ui_extension_gets_its_own_group_and_item() {
        let tmp = TempDir::new("ext-items-ui");
        tmp.write(
            "ui/extension.json",
            r#"{ "id": "ui", "name": "UI 示例", "kind": "ui", "entry": "main.js" }"#,
        );
        tmp.write("ui/main.js", "");

        let (items, errors) = items_in(&tmp.0);
        assert!(errors.is_empty());
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].id, "ui.ui");
        assert_eq!(items[0].title, "UI 示例");
        assert_eq!(items[0].group, "扩展");
    }

    #[test]
    fn broken_manifest_is_reported_in_errors() {
        let tmp = TempDir::new("ext-items-broken");
        // kind 是 ui 但没写 main.js 入口 → UI 侧报错。
        tmp.write("bad/extension.json", r#"{ "id": "bad", "kind": "ui" }"#);

        let (items, errors) = items_in(&tmp.0);
        assert!(items.is_empty());
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("bad"));
    }
}
