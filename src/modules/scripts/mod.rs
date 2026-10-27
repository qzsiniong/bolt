//! The scripts module: the script manager window (dock with script list,
//! editor and console) plus one launcher item per script on disk.

pub(crate) mod panels;
pub(crate) mod state;
pub(crate) mod window;
pub(crate) mod workspace;

use std::path::Path;
use std::sync::Arc;

use gpui_kit::App;
use gpui_kit::assets::IconName;

use crate::app::launcher::{ItemAction, LauncherAction, LauncherItem, Module, PasteHandler};
use crate::model::script::Language;
use crate::model::script_store;
use crate::ui::clipboard::PasteContent;
use crate::ui::mac_ffi;

/// Scripts 模块：提供「脚本管理」「新建脚本」两条内置命令，以及脚本目录里
/// 每个脚本一条的运行命令。
pub(crate) struct ScriptsModule;

impl Module for ScriptsModule {
    fn name(&self) -> &'static str {
        "scripts"
    }

    fn collect_items(&self, _cx: &App) -> Vec<LauncherItem> {
        items_in(&script_store::scripts_dir())
    }
}

/// 纯函数：扫描 `scripts_dir` 产出 items（内置两条 + 每个脚本一条）。
/// 与 App 上下文解耦，方便单测。
fn items_in(scripts_dir: &Path) -> Vec<LauncherItem> {
    let mut items = vec![
        LauncherItem {
            id: "scripts.open-manager".to_string(),
            title: "脚本管理".to_string(),
            subtitle: Some("打开脚本管理器窗口".to_string()),
            icon: IconName::FolderCog.into(),
            group: "内置".to_string(),
            keywords: vec![String::from("jbgl"), String::from("bolt")],
            on_confirm: Arc::new(|cx| window::open_manager(cx, false)),
            actions: Vec::new(),
            paste: None,
        },
        LauncherItem {
            id: "scripts.new".to_string(),
            title: "新建脚本".to_string(),
            subtitle: Some("在脚本管理器中创建新脚本".to_string()),
            icon: IconName::FilePlus.into(),
            group: "内置".to_string(),
            keywords: Vec::new(),
            on_confirm: Arc::new(|cx| window::open_manager(cx, true)),
            actions: Vec::new(),
            paste: None,
        },
    ];

    for script in script_store::list_scripts(scripts_dir) {
        let path = script.path.clone();
        let primary: LauncherAction = Arc::new({
            let path = path.clone();
            move |cx| crate::runtime::run_path(cx, &path)
        });
        // Cmd+K 动作：运行 / 在 Finder 中显示 / 复制路径。
        let reveal_path = path.clone();
        let copy_path = path.to_string_lossy().into_owned();
        items.push(LauncherItem {
            id: format!("scripts.run:{}", script.file_name),
            title: script.name,
            subtitle: Some(script.file_name),
            icon: IconName::FileCode.into(),
            group: "脚本".to_string(),
            keywords: Vec::new(),
            on_confirm: primary.clone(),
            actions: vec![
                ItemAction {
                    title: "运行".to_string(),
                    shortcut: Some("↵"),
                    run: primary,
                    dialog: None,
                },
                ItemAction {
                    title: "在 Finder 中显示".to_string(),
                    shortcut: None,
                    run: Arc::new(move |_| match mac_ffi::reveal_in_finder(&reveal_path) {
                        Ok(()) => mac_ffi::notify(
                            "Bolt",
                            &format!(
                                "已在 Finder 中显示 {}",
                                reveal_path
                                    .file_name()
                                    .map(|s| s.to_string_lossy().into_owned())
                                    .unwrap_or_default()
                            ),
                        ),
                        Err(e) => mac_ffi::notify("Bolt", &format!("显示失败：{e}")),
                    }),
                    dialog: None,
                },
                ItemAction {
                    title: "复制路径".to_string(),
                    shortcut: None,
                    run: Arc::new(move |_| match mac_ffi::copy_text(&copy_path) {
                        Ok(()) => mac_ffi::notify("Bolt", "路径已复制到剪贴板"),
                        Err(e) => mac_ffi::notify("Bolt", &format!("复制失败：{e}")),
                    }),
                    dialog: None,
                },
            ],
            // 粘贴一个脚本文件（Finder 复制 .py/.sh/…）时直接运行。
            paste: Some(PasteHandler {
                accepts: |content: &PasteContent| {
                    matches!(
                        content,
                        PasteContent::FilePath(p) if Language::from_path(p.as_path()).is_some()
                    )
                },
                run: Arc::new(|cx: &mut App, content: &PasteContent| {
                    if let PasteContent::FilePath(path) = content {
                        crate::runtime::run_path(cx, path.as_path());
                    }
                }),
            }),
        });
    }
    items
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TempDir;

    #[test]
    fn empty_dir_yields_only_builtin_items() {
        let tmp = TempDir::new("scripts-items-empty");
        let items = items_in(&tmp.0);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].id, "scripts.open-manager");
        assert_eq!(items[1].id, "scripts.new");
        assert!(items.iter().all(|item| item.group == "内置"));
    }

    #[test]
    fn each_script_becomes_a_run_item() {
        let tmp = TempDir::new("scripts-items-full");
        tmp.write("alpha.js", "console.log(1);");
        tmp.write("beta.ts", "console.log(2);");

        let items = items_in(&tmp.0);
        assert_eq!(items.len(), 4);
        let runs: Vec<&LauncherItem> = items.iter().filter(|item| item.group == "脚本").collect();
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].id, "scripts.run:alpha.js");
        assert_eq!(runs[0].subtitle.as_deref(), Some("alpha.js"));
        assert_eq!(runs[1].id, "scripts.run:beta.ts");
    }
}
