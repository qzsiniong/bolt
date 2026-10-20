//! Window lifecycle: the launcher palette and the manager window, plus the
//! dispatch of confirmed launcher commands.

use std::path::Path;

use crossbeam_channel::Receiver;
use gpui_kit::component::TitleBar;
use gpui_kit::*;

use super::shell::AppShell;
use crate::model::script::Language;
use crate::modules::json_editor::panel::JsonEditorPanel;
use crate::modules::launcher::{Launcher, LauncherRequest};
use crate::modules::scripts::Workspace;

/// Launcher window size.
const LAUNCHER_SIZE: (f32, f32) = (720., 480.);

/// The display under the mouse cursor, so with multiple monitors the launcher
/// opens on the screen the cursor is on. `None` when it cannot be determined;
/// callers then fall back to the primary display.
#[cfg(target_os = "macos")]
fn display_under_cursor() -> Option<DisplayId> {
    use core_graphics::display::{CGDirectDisplayID, CGDisplayBounds, CGGetActiveDisplayList};
    use core_graphics::event::CGEvent;
    use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};

    // A freshly synthesized event reports the current cursor location in the
    // global display coordinate space (origin: top-left of the primary
    // display). Creating an event source needs no accessibility permission.
    let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState).ok()?;
    let point = CGEvent::new(source).ok()?.location();

    // Same 32-display upper bound gpui's own platform code assumes.
    let mut ids: Vec<CGDirectDisplayID> = Vec::with_capacity(32);
    let mut count = 0;
    // SAFETY: the buffer has the capacity passed in; `count` says how many
    // entries were written.
    if unsafe { CGGetActiveDisplayList(ids.capacity() as u32, ids.as_mut_ptr(), &mut count) } != 0 {
        return None;
    }
    unsafe { ids.set_len(count as usize) };

    ids.into_iter()
        .find(|&id| {
            // SAFETY: FFI call with a valid active display id.
            let frame = unsafe { CGDisplayBounds(id) };
            point.x >= frame.origin.x
                && point.x < frame.origin.x + frame.size.width
                && point.y >= frame.origin.y
                && point.y < frame.origin.y + frame.size.height
        })
        .map(|id| DisplayId::new(id as u64))
}

#[cfg(not(target_os = "macos"))]
fn display_under_cursor() -> Option<DisplayId> {
    None
}

/// Alt+Space: close the launcher if it is open, open it otherwise. A stale
/// handle means the window closed itself (Esc / blur / confirm) — reopen.
pub(super) fn toggle_launcher(cx: &mut App) {
    let existing = cx.global_mut::<AppShell>().launcher.take();
    if let Some(handle) = existing
        && handle
            .update(cx, |_, window, _| window.remove_window())
            .is_ok()
    {
        return;
    }
    // Stale handle: fall through and reopen.

    let (requests, console) = {
        let shell = cx.global::<AppShell>();
        (shell.launcher_tx.clone(), shell.console_channel.clone())
    };
    // Center on the display under the cursor; falls back to the primary
    // display when it can't be determined.
    let display_id = display_under_cursor();
    let window_size = size(px(LAUNCHER_SIZE.0), px(LAUNCHER_SIZE.1));
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
            display_id,
            window_size,
            cx,
        ))),
        display_id,
        titlebar: None,
        kind: WindowKind::PopUp,
        is_resizable: false,
        focus: true,
        ..Default::default()
    };
    let opened = gpui_kit::open_window(options, cx, |window, cx| {
        cx.new(|cx| Launcher::new(window, cx, requests, console))
    });
    match opened {
        Ok((handle, _view)) => {
            cx.global_mut::<AppShell>().launcher = Some(handle);
        }
        Err(e) => {
            cx.global::<AppShell>()
                .console_channel
                .warn(format!("无法打开启动器窗口：{e}"));
        }
    }
}

/// Read a script or extension entry from disk and hand it to the run service.
pub(super) fn run_path(cx: &mut App, path: &Path) {
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

/// Open the manager window as a singleton: focus the existing one (and
/// optionally start its new-script flow), or create a fresh one.
pub(super) fn open_manager(cx: &mut App, new_script: bool) {
    // The launcher (a PopUp window, window level 101) has already closed by the
    // time this runs, so the app briefly has no window and macOS hands
    // activation back to the previous app. gpui only calls makeKeyAndOrderFront
    // when creating a window and never NSApp.activate, so the Normal-level
    // manager window ends up behind the other app until the app is reactivated
    // (e.g. by clicking the Dock icon). Activate explicitly, which is exactly
    // what that Dock click does.
    cx.activate(true);

    let existing = cx.global_mut::<AppShell>().manager.take();
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
            cx.global_mut::<AppShell>().manager = Some((handle, workspace));
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
            cx.global_mut::<AppShell>().manager = Some((handle, workspace.downgrade()));
        }
        Err(e) => {
            console.error(format!("无法打开脚本管理器窗口：{e}"));
        }
    }
}

/// Open the native JSON editor window as a singleton — focus the existing
/// handle if it's still alive, otherwise activate the app and create a fresh
/// one. Mirrors `open_manager` above.
pub(super) fn open_json_editor(cx: &mut App) {
    cx.activate(true);

    let existing = cx.global_mut::<AppShell>().json_editor.take();
    if let Some(handle) = existing {
        let alive = handle
            .update(cx, |_, window, _| window.activate_window())
            .is_ok();
        if alive {
            cx.global_mut::<AppShell>().json_editor = Some(handle);
            return;
        }
    }

    let console = cx.global::<AppShell>().console_channel.clone();
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
            None,
            size(px(880.), px(720.)),
            cx,
        ))),
        kind: WindowKind::Normal,
        window_min_size: Some(size(px(520.), px(380.))),
        titlebar: Some(TitlebarOptions {
            title: Some("JSON 编辑器".into()),
            ..TitleBar::title_bar_options()
        }),
        is_resizable: true,
        focus: true,
        // 以 `TitleBar::window_options()` 为基底：标题栏由视图自绘（含右上角
        // 置顶图钉），拖动/双击交给 `TitleBar` 自己处理，AppKit 不再把它当
        // 系统拖动区。
        ..TitleBar::window_options()
    };

    let opened = gpui_kit::open_window(options, cx, |window, cx| {
        cx.new(|cx| JsonEditorPanel::new(window, cx))
    });
    match opened {
        Ok((handle, _panel)) => {
            cx.global_mut::<AppShell>().json_editor = Some(handle);
        }
        Err(e) => {
            console.error(format!("无法打开 JSON 编辑器窗口：{e}"));
        }
    }
}

/// Answer launcher commands from the app tick.
///
/// Dispatch happens here rather than in the launcher's own confirm callback:
/// that callback runs while the launcher window is being torn down, which
/// drops the emitting view before a deferred entity event could reach it.
pub(super) fn drain_launcher_requests(rx: &Receiver<LauncherRequest>, cx: &mut App) {
    while let Ok(request) = rx.try_recv() {
        match request {
            LauncherRequest::OpenManager { new_script } => open_manager(cx, new_script),
            LauncherRequest::OpenJsonEditor => open_json_editor(cx),
            LauncherRequest::RunFile(path) => run_path(cx, &path),
            LauncherRequest::OpenExtensionUi { ext } => {
                super::extension_ui::open_extension_ui(cx, ext)
            }
        }
    }
}
