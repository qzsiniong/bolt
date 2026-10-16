//! Synthetic keystrokes for `typeStr`.
//!
//! Uses enigo for cross-platform keyboard simulation. Unicode characters go
//! through `text()` (handles CJK and arbitrary keyboard layouts); Return / Tab
//! use explicit key presses.
//!
//! macOS wraps the `Enigo` in `ManuallyDrop` so its default Drop (which
//! accumulates sleeps and can block for seconds on long texts) never runs.
//! We still release Shift on the way out to avoid leaving a stuck modifier.

use std::mem::ManuallyDrop;
use std::time::{Duration, Instant};

use enigo::{Direction, Enigo, Key, Keyboard, Settings};

use crate::runtime::interaction::{ABORTED, InteractionBridge, ProgressSnapshot};

/// Standby text used when the script starts typing without asking for a target.
pub const DEFAULT_FOCUS_HINT: &str = "请点击要输入文本的目标窗口（本窗口失去焦点后自动继续）";

/// Default inter-key delay. Long enough that the target app keeps up but short
/// enough to feel responsive.
pub const DEFAULT_DELAY_MS: u64 = 300;

const DEFAULT_LABEL: &str = "输入文本";

const INIT_FAIL_HINT: &str = "无法初始化键盘模拟：请检查「系统设置 → 隐私与安全性 → 辅助功能」中是否允许 Scripter（修改后需重启应用）";

/// `typeStr(text, delayMs, label?)`.
///
/// Synchronous by design: each character is posted before the script continues,
/// so output order matches execution order. Abort is noticed at the next
/// character boundary.
pub fn type_str(
    text: &str,
    delay_ms: u64,
    label: Option<&str>,
    bridge: &InteractionBridge,
) -> Result<(), String> {
    if bridge.cancel_token().is_cancelled() {
        return Err(ABORTED.to_string());
    }
    // First output of the run: let the user pick where it goes. Without this
    // the keys land in whatever window happens to be focused — usually Scripter.
    if !bridge.take_focus_confirmation() {
        bridge.request_focus(DEFAULT_FOCUS_HINT.to_string())?;
    }

    let label = label.unwrap_or(DEFAULT_LABEL);
    let result = type_str_inner(text, delay_ms, label, bridge);
    // Never leave a stale snapshot behind: the UI closes its window as soon as
    // the slot is empty, including on an aborted call.
    bridge.set_progress(None);
    result
}

fn type_str_inner(
    text: &str,
    delay_ms: u64,
    label: &str,
    bridge: &InteractionBridge,
) -> Result<(), String> {
    let enigo = Enigo::new(&Settings::default()).map_err(|e| format!("{INIT_FAIL_HINT}：{e}"))?;
    // `ManuallyDrop` skips enigo's own Drop, which on macOS accumulates sleeps
    // from prior `update_wait_time` calls and blocks for seconds on long texts.
    // We still clean up modifiers explicitly below.
    let mut enigo = ManuallyDrop::new(enigo);

    let total = text.chars().count() as u64;
    let delay = Duration::from_millis(delay_ms);
    let started = Instant::now();

    // Throttle: only write a snapshot once per elapsed second. Writing more
    // often churns the UI and makes ETA estimates wobble.
    let mut last_reported_secs: u64 = u64::MAX;
    let mut report = |current: u64| {
        let elapsed_ms = started.elapsed().as_millis() as u64;
        let secs = elapsed_ms / 1000;
        if secs != last_reported_secs || current == total {
            last_reported_secs = secs;
            bridge.set_progress(Some(ProgressSnapshot {
                label: label.to_string(),
                current,
                total,
                elapsed_ms,
            }));
        }
    };

    let mut result: Result<(), String> = Ok(());
    for (index, ch) in text.chars().enumerate() {
        if bridge.cancel_token().is_cancelled() {
            result = Err(ABORTED.to_string());
            break;
        }
        if let Err(e) = post_char(&mut enigo, ch) {
            result = Err(format!("按键事件发送失败：{e}"));
            break;
        }

        report(index as u64 + 1);

        if !delay.is_zero() {
            std::thread::sleep(delay);
        }
    }

    // Release Shift so we don't leave it stuck across apps — possible if the
    // operation was cancelled mid-combo. Best-effort; ignore errors.
    let _ = enigo.key(Key::Shift, Direction::Release);
    // Deliberately do NOT call `into_inner()` — that would unwrap ManuallyDrop
    // and let Enigo's real Drop run, which accumulates sleeps and blocks for
    // seconds. ManuallyDrop has no Drop impl, so just letting it go out of
    // scope is the entire point of wrapping it.
    result
}

/// Post a single key press for `ch`.
///
/// `\n` / `\r` → Return, `\t` → Tab, everything else rides through `text()`
/// which handles arbitrary Unicode (including CJK) without a layout-dependent
/// keymap lookup.
fn post_char(enigo: &mut Enigo, ch: char) -> Result<(), String> {
    match ch {
        '\n' | '\r' => enigo.key(Key::Return, Direction::Click),
        '\t' => enigo.key(Key::Tab, Direction::Click),
        _ => enigo.text(&ch.to_string()),
    }
    .map_err(|e| format!("{e}"))
}
