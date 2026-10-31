//! 脚本执行管线：QuickJS 运行器、宿主交互桥与应用级运行服务。
//! TypeScript 类型擦除在 `bolt-ts` crate。

use std::path::Path;

use gpui_kit::App;

use crate::app::shell::AppShell;
use bolt_model::script::Language;

pub(crate) mod interaction;
pub(crate) mod interaction_window_manager;
pub(crate) mod run_service;
pub(crate) mod runner;

/// 从磁盘读取脚本或扩展入口文件，再交给运行服务执行。
/// 启动器里脚本模块和扩展模块的条目都走这里。
pub(crate) fn run_path(cx: &mut App, path: &Path) {
    let console = cx.global::<AppShell>().console_channel.clone();
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("script.js")
        .to_string();
    let Some(language) = Language::from_path(path) else {
        console.error(format!("无法识别的脚本类型：{}", path.display()));
        return;
    };
    let content = match std::fs::read_to_string(path) {
        Ok(content) => content,
        Err(e) => {
            console.error(format!("读取脚本失败 {}：{e}", path.display()));
            return;
        }
    };
    let run_service = cx.global::<AppShell>().run_service.clone();
    run_service.update(cx, |svc, cx| {
        svc.start_script(content, language, file_name, cx)
    });
}
