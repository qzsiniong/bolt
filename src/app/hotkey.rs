//! The Raycast-style global hotkey: registration and polling.
//!
//! `global_hotkey` delivers presses on a crossbeam channel, so the app polls
//! it from the same tick that drains launcher requests.

use global_hotkey::hotkey::{Code, HotKey, Modifiers};
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};

/// Register Alt+Space as the launcher toggle.
///
/// Returns the manager (which must be kept alive for the app lifetime —
/// dropping it unregisters the hotkey) and a diagnostics result for the
/// Console.
pub(super) fn register() -> (Option<GlobalHotKeyManager>, Result<(), String>) {
    match GlobalHotKeyManager::new() {
        Ok(manager) => {
            let hotkey = HotKey::new(Some(Modifiers::ALT), Code::Space);
            match manager.register(hotkey) {
                Ok(()) => (Some(manager), Ok(())),
                Err(e) => (None, Err(format!("注册全局快捷键失败：{e}"))),
            }
        }
        Err(e) => (None, Err(format!("初始化全局快捷键失败：{e}"))),
    }
}

/// True once per hotkey press since the last call.
pub(super) fn poll_pressed() -> bool {
    let mut pressed = false;
    while let Ok(event) = GlobalHotKeyEvent::receiver().try_recv() {
        if event.state == HotKeyState::Pressed {
            pressed = true;
        }
    }
    pressed
}
