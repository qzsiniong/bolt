//! Global hotkey registration and polling.
//!
//! `global_hotkey` delivers presses on a crossbeam channel, so the app polls
//! them from the same tick that drains launcher requests. Each registered
//! hotkey carries a `HotKeyId` so one `poll_pressed` call can report which
//! hotkey(s) fired — the app tick dispatches accordingly.

use std::sync::OnceLock;

use global_hotkey::hotkey::{Code, HotKey, Modifiers};
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};

/// 已注册快捷键的 `HotKey::id()` → [`HotKeyId`]，由 [`register`] 填一次。
///
/// 必须按**实际 id** 建表：`GlobalHotKeyEvent.id` 不是注册序号，而是
/// `HotKey::new` 算出的哈希 `(mods.bits() << 16) | key as u32`
/// （global-hotkey 0.8.0 的 `hotkey.rs` L97），硬编码 0/1 永远匹配不上，
/// 事件会全被丢弃 —— 快捷键因此毫无反应。
static HOTKEY_IDS: OnceLock<Vec<(u32, HotKeyId)>> = OnceLock::new();

/// Distinguishes which of the app's registered hotkeys fired.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HotKeyId {
    /// Alt+Space → 唤起 / 关闭 Launcher。
    Launcher,
    /// Cmd+Shift+T → Pin 当前剪切板内容。
    PinClipboard,
}

/// 单个快捷键的注册结果：`HotKeyId` 配上它注册成功或失败的原因。
pub(super) type HotKeyRegistration = (HotKeyId, Result<(), String>);

/// [`register`] 的返回值：管理器（None = 初始化失败）+ 每个快捷键的注册结果。
pub(super) type HotKeyRegistrations = (Option<GlobalHotKeyManager>, Vec<HotKeyRegistration>);

/// Register both built-in hotkeys (Alt+Space, Cmd+Shift+T) and keep the
/// manager alive for the app lifetime. Returns per-hotkey registration
/// results so the Console can report what went wrong.
pub(super) fn register() -> HotKeyRegistrations {
    match GlobalHotKeyManager::new() {
        Ok(manager) => {
            let registrations = vec![
                (
                    HotKeyId::Launcher,
                    HotKey::new(Some(Modifiers::ALT), Code::Space),
                ),
                (
                    HotKeyId::PinClipboard,
                    HotKey::new(Some(Modifiers::SUPER | Modifiers::SHIFT), Code::KeyT),
                ),
            ];
            let mut results = Vec::with_capacity(registrations.len());
            let mut ids = Vec::with_capacity(registrations.len());
            for (id, hotkey) in registrations {
                ids.push((hotkey.id(), id));
                match manager.register(hotkey) {
                    Ok(()) => results.push((id, Ok(()))),
                    Err(e) => results.push((id, Err(format!("注册全局快捷键失败：{e}")))),
                }
            }
            let _ = HOTKEY_IDS.set(ids);
            (Some(manager), results)
        }
        Err(e) => {
            let err = format!("初始化全局快捷键管理器失败：{e}");
            (
                None,
                vec![
                    (HotKeyId::Launcher, Err(err.clone())),
                    (HotKeyId::PinClipboard, Err(err)),
                ],
            )
        }
    }
}

/// Return every `HotKeyId` whose hotkey was pressed since the last call.
/// Each press is reported at most once — safe to call repeatedly from a tick.
pub(super) fn poll_pressed() -> Vec<HotKeyId> {
    let Some(ids) = HOTKEY_IDS.get() else {
        return Vec::new();
    };
    let mut fired = Vec::new();
    while let Ok(event) = GlobalHotKeyEvent::receiver().try_recv() {
        if event.state != HotKeyState::Pressed {
            continue;
        }
        let Some(id) = resolve_hotkey(ids, event.id) else {
            continue;
        };
        if !fired.contains(&id) {
            fired.push(id);
        }
    }
    fired
}

/// 把 `GlobalHotKeyEvent.id`（哈希值）翻译成 [`HotKeyId`]。
fn resolve_hotkey(ids: &[(u32, HotKeyId)], event_id: u32) -> Option<HotKeyId> {
    ids.iter()
        .find(|(hotkey_id, _)| *hotkey_id == event_id)
        .map(|(_, id)| *id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hotkey_ids_are_hashes_not_sequence_numbers() {
        // 曾经硬编码 `0 => Launcher, 1 => PinClipboard`，但真实 id 是
        // `(mods << 16) | key` 的哈希，于是所有事件都被 `_ => continue` 丢掉，
        // 快捷键完全无反应。这里锁死这条回归。
        let alt_space = HotKey::new(Some(Modifiers::ALT), Code::Space);
        let cmd_shift_t = HotKey::new(Some(Modifiers::SUPER | Modifiers::SHIFT), Code::KeyT);
        assert_ne!(alt_space.id(), 0);
        assert_ne!(cmd_shift_t.id(), 1);

        let ids = vec![
            (alt_space.id(), HotKeyId::Launcher),
            (cmd_shift_t.id(), HotKeyId::PinClipboard),
        ];
        assert_eq!(
            resolve_hotkey(&ids, alt_space.id()),
            Some(HotKeyId::Launcher)
        );
        assert_eq!(
            resolve_hotkey(&ids, cmd_shift_t.id()),
            Some(HotKeyId::PinClipboard)
        );
        assert_eq!(resolve_hotkey(&ids, 0), None);
    }
}
