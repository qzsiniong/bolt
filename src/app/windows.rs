//! Window lifecycle: the launcher palette, plus the dispatch of confirmed
//! launcher item actions. Module windows (script manager, JSON editor, UI
//! extensions) open themselves — see each module's `window`/`ui_window` file.

use crossbeam_channel::Receiver;
use gpui_kit::*;

use super::launcher::{Launcher, LauncherAction};
use super::shell::AppShell;

/// Launcher window size.
const LAUNCHER_SIZE: (f32, f32) = (720., 480.);

/// The display under the mouse cursor, so with multiple monitors the launcher
/// opens on the screen the cursor is on. `None` when it cannot be determined;
/// callers then fall back to the primary display.
#[cfg(target_os = "macos")]
pub(crate) fn display_under_cursor() -> Option<DisplayId> {
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
pub(crate) fn display_under_cursor() -> Option<DisplayId> {
    None
}

/// 显式激活当前 App（等价于用户点击 Dock 图标）。
///
/// `WindowKind::Floating` 的 NSPanel（不像 `PopUp` 的 NonactivatingPanel）
/// 只有在应用已激活时才会浮到前台。gpui-pre-macos 的 `open_window` 里
/// 的 `makeKeyAndOrderFront_` 只调窗口级方法，不调 `[NSApp activate]`，
/// 所以从全局快捷键（App 非激活态）唤起时要手动补这一步。
#[cfg(target_os = "macos")]
fn activate_app() {
    use objc2::msg_send;
    use objc2::runtime::AnyClass;

    unsafe {
        // sharedApplication 是单例，永远不会返回 nil；忽略返回值。
        let app_class = match AnyClass::get(c"NSApplication") {
            Some(c) => c,
            None => return,
        };
        let app: *mut objc2::runtime::AnyObject = msg_send![app_class, sharedApplication];
        if app.is_null() {
            return;
        }
        let _: () = msg_send![app, activateIgnoringOtherApps: true];
    }
}

#[cfg(not(target_os = "macos"))]
fn activate_app() {
    // Linux/Windows 下 Floating 窗口的激活策略由平台层统一处理，不需要额外调用。
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

    let requests = cx.global::<AppShell>().launcher_tx.clone();
    // Floating 窗口只有在应用已经激活时才会出现在前台；全局快捷键按下时
    // 很可能是别的 App 在前台，必须先显式 activateIgnoringOtherApps:YES，
    // 否则窗口会被压住看不见（但其实已经创建了——之前的 PopUp 因为带
    // NSWindowStyleMaskNonactivatingPanel + NSPopUpWindowLevel=101 才绕过了
    // 这个问题，代价是 IME 不工作）。
    activate_app();
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
        kind: WindowKind::Floating,
        is_resizable: false,
        focus: true,
        ..Default::default()
    };
    let opened = gpui_kit::open_window(options, cx, |window, cx| {
        cx.new(|cx| Launcher::new(window, cx, requests))
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

/// Execute confirmed launcher item actions from the app tick.
///
/// Dispatch happens here rather than in the launcher's own confirm callback:
/// that callback runs while the launcher window is being torn down, so the
/// item's action (an `Arc` closure that owns everything it needs) travels
/// through this channel to outlive the launcher entity.
pub(super) fn drain_launcher_requests(rx: &Receiver<LauncherAction>, cx: &mut App) {
    while let Ok(action) = rx.try_recv() {
        action(cx);
    }
}

/// 模块的 items 变了（例如应用图标刚生成完）：记一次 revision，并让已经
/// 打开的 Launcher 在下一帧重新收集。没有打开的 Launcher 什么都不用做——
/// 它下次唤起时本来就会重新 collect。
pub(crate) fn items_changed(cx: &mut App) {
    let launcher = {
        let shell = cx.global::<AppShell>();
        shell
            .items_revision
            .set(shell.items_revision.get().wrapping_add(1));
        shell.launcher
    };
    if let Some(handle) = launcher {
        // 窗口已关闭时 update 会失败，忽略即可：stale 的 handle 由
        // `toggle_launcher` 负责清理。
        let _ = handle.update(cx, |_, window, _| window.refresh());
    }
}
