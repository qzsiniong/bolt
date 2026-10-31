//! 脚本命令的执行：懒加载扩展 → 交给 `RunService` 在工作线程里调。
//!
//! 加载（建运行时、求值入口模块）必须发生在主线程：`plux::Loader` 是 `&mut`
//! 单线程的。真正的命令执行则在 RunService 的工作线程里，UI 不会被卡住。

use gpui_kit::App;

use super::host::CommandTarget;
use super::state::ExtensionsState;
use crate::app::shell::AppShell;

pub(crate) fn run_command(cx: &mut App, target: CommandTarget) {
    let console = cx.global::<AppShell>().console_channel.clone();
    let label = format!("{} · {}", target.extension_name, target.title);

    let loaded = cx
        .global::<ExtensionsState>()
        .host
        .borrow_mut()
        .ensure_loaded(&target);
    if let Err(error) = loaded {
        console.error(format!("扩展加载失败 {}：{error}", target.extension_id));
        return;
    }

    let run_service = cx.global::<AppShell>().run_service.clone();
    run_service.update(cx, |service, cx| {
        service.start_extension(target.bundle.clone(), target.command_id.clone(), label, cx)
    });
}
