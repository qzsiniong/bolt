//! The script execution pipeline: the QuickJS runner, the host-interaction
//! bridge, the app-level run service, and the TypeScript type-stripping step.

use std::path::Path;

use gpui_kit::App;

use crate::app::shell::AppShell;
use crate::model::script::Language;

pub(crate) mod interaction;
pub(crate) mod run_service;
pub(crate) mod runner;
pub(crate) mod ts_transform;

/// Read a script or extension entry from disk and hand it to the run service.
/// Used by launcher items from both the scripts and extensions modules.
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
    run_service.update(cx, |svc, cx| svc.start(content, language, file_name, cx));
}
