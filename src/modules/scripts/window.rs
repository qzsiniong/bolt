//! 脚本管理器窗口的开窗逻辑（单例）。

use gpui_kit::{App, AppContext as _, WindowOptions};

use super::state::ScriptsState;
use super::workspace::Workspace;
use crate::app::shell::AppShell;

/// Open the manager window as a singleton: focus the existing one (and
/// optionally start its new-script flow), or create a fresh one.
pub(crate) fn open_manager(cx: &mut App, new_script: bool) {
    // The launcher (a PopUp window, window level 101) has already closed by the
    // time this runs, so the app briefly has no window and macOS hands
    // activation back to the previous app. gpui only calls makeKeyAndOrderFront
    // when creating a window and never NSApp.activate, so the Normal-level
    // manager window ends up behind the other app until the app is reactivated
    // (e.g. by clicking the Dock icon). Activate explicitly, which is exactly
    // what that Dock click does.
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
        // Dead window: fall through and create a new one.
    }

    let (lsp, console_rx, run_service, console) = {
        let shell = cx.global::<AppShell>();
        (
            shell.lsp.clone(),
            shell.console_rx.clone(),
            shell.run_service.clone(),
            shell.console_channel.clone(),
        )
    };
    let opened = gpui_kit::open_window(WindowOptions::default(), cx, |window, cx| {
        cx.new(|cx| Workspace::new(window, cx, lsp, console_rx, run_service))
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
