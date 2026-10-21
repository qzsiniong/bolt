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

use crate::app::launcher::{LauncherItem, Module};
use crate::model::script_store;

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
            on_confirm: Arc::new(|cx| window::open_manager(cx, false)),
        },
        LauncherItem {
            id: "scripts.new".to_string(),
            title: "新建脚本".to_string(),
            subtitle: Some("在脚本管理器中创建新脚本".to_string()),
            icon: IconName::FilePlus.into(),
            group: "内置".to_string(),
            on_confirm: Arc::new(|cx| window::open_manager(cx, true)),
        },
    ];

    for script in script_store::list_scripts(scripts_dir) {
        let path = script.path.clone();
        items.push(LauncherItem {
            id: format!("scripts.run:{}", script.file_name),
            title: script.name,
            subtitle: Some(script.file_name),
            icon: IconName::FileCode.into(),
            group: "脚本".to_string(),
            on_confirm: Arc::new(move |cx| crate::runtime::run_path(cx, &path)),
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
