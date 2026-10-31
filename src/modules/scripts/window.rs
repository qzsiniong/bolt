//! 脚本管理器窗口的开窗逻辑（单例）。

use gpui_kit::{App, AppContext as _};

use super::state::ScriptsState;
use super::workspace::Workspace;
use crate::app::shell::AppShell;
use crate::ui::window::{WindowSpec, open_spec};

/// 以单例方式打开管理器窗口：已有窗口就激活它（可选顺带启动新建脚本
/// 流程），否则新建一个。
pub(crate) fn open_manager(cx: &mut App, new_script: bool) {
    // 执行到这里时 Launcher（PopUp 窗口，window level 101）已经关了，应用
    // 短暂处于无窗口状态，macOS 会把激活权交还给上一个应用。gpui 只在
    // 创建窗口时调 makeKeyAndOrderFront，从不调 NSApp.activate，于是
    // Normal 层级的管理器窗口会一直被压在别的应用下面，直到应用重新被
    // 激活（比如点 Dock 图标）。这里显式 activate——与点 Dock 图标等效。
    cx.activate(true);

    let existing = cx.global_mut::<ScriptsState>().manager.take();
    if let Some((handle, workspace)) = existing {
        let activated = handle
            .update(cx, |_, window, cx| {
                window.activate_window();
                if new_script && let Some(workspace) = workspace.upgrade() {
                    workspace.update(cx, |ws, cx| ws.trigger_new_script(window, cx));
                }
            })
            .is_ok();
        if activated {
            cx.global_mut::<ScriptsState>().manager = Some((handle, workspace));
            return;
        }
        // 窗口已死：继续往下走，新建一个。
    }

    let (lsp, console_log, run_service, console) = {
        let shell = cx.global::<AppShell>();
        (
            shell.lsp.clone(),
            shell.console_channel.history(),
            shell.run_service.clone(),
            shell.console_channel.clone(),
        )
    };
    // 不设 size：沿用系统默认的开窗位置与尺寸（原来 `WindowOptions::default()`
    // 的行为）。激活已有窗口要带副作用（触发新建脚本流程），所以不走
    // `open_singleton`，自管既有句柄、只用 `open_spec` 开新窗。
    let opened = open_spec(&WindowSpec::new("Bolt"), cx, |window, cx| {
        cx.new(|cx| Workspace::new(window, cx, lsp, console_log, run_service))
    });
    match opened {
        Ok((handle, workspace)) => {
            if new_script {
                let _ = handle.update(cx, |_, window, cx| {
                    workspace.update(cx, |ws, cx| ws.trigger_new_script(window, cx));
                });
            }
            cx.global_mut::<ScriptsState>().manager = Some((handle, workspace.downgrade()));
        }
        Err(e) => {
            console.error(format!("无法打开脚本管理器窗口：{e}"));
        }
    }
}
