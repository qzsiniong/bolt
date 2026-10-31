//! 为 `typeStr` 模拟键盘输入。
//!
//! 用 enigo 做跨平台按键模拟。Unicode 字符走 `text()`（能处理 CJK 和任意键盘
//! 布局），Return / Tab 用显式按键。
//!
//! macOS 上把 `Enigo` 包进 `ManuallyDrop`，让它默认的 Drop 永远不跑（那个 Drop
//! 会累积 sleep，长文本时能卡住好几秒）。退出前仍然显式松开 Shift，避免留下卡住的
//! 修饰键。

use std::mem::ManuallyDrop;
use std::time::{Duration, Instant};

use enigo::{Direction, Enigo, Key, Keyboard, Settings};

use crate::runtime::interaction::{ABORTED, InteractionBridge, ProgressSnapshot};

/// 脚本没指定目标就开始输入时展示的待机提示。
pub const DEFAULT_FOCUS_HINT: &str = "请点击要输入文本的目标窗口（本窗口失去焦点后自动继续）";

/// 默认的字符间隔。长到目标应用跟得上，又短到来得及响应。
pub const DEFAULT_DELAY_MS: u64 = 300;

const DEFAULT_LABEL: &str = "输入文本";

const INIT_FAIL_HINT: &str = "无法初始化键盘模拟：请检查「系统设置 → 隐私与安全性 → 辅助功能」中是否允许 Bolt（修改后需重启应用）";

/// `typeStr(text, delayMs, label?)`。
///
/// 刻意做成同步的：每个字符都先发出去脚本才继续，所以输出顺序与执行顺序一致。
/// 中止会在下一个字符边界被发现。
pub fn type_str(
    text: &str,
    delay_ms: u64,
    label: Option<&str>,
    bridge: &InteractionBridge,
) -> Result<(), String> {
    if bridge.cancel_token().is_cancelled() {
        return Err(ABORTED.to_string());
    }
    // 本次运行的第一次输出：先让用户选定落到哪个窗口。否则按键会打进当时恰好
    // 聚焦的窗口——通常就是 Bolt 自己。
    if !bridge.take_focus_confirmation() {
        bridge.request_focus(DEFAULT_FOCUS_HINT.to_string())?;
    }

    let label = label.unwrap_or(DEFAULT_LABEL);
    let result = type_str_inner(text, delay_ms, label, bridge);
    // 别留下过期快照：槽位一空 UI 就会关掉进度窗，中止路径同样如此。
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
    // `ManuallyDrop` 跳过 enigo 自己的 Drop：macOS 上它会把之前
    // `update_wait_time` 累积的 sleep 一起睡完，长文本时卡住数秒。
    // 修饰键仍在下面显式清理。
    let mut enigo = ManuallyDrop::new(enigo);

    let total = text.chars().count() as u64;
    let delay = Duration::from_millis(delay_ms);
    let started = Instant::now();

    // 限流：每过一秒才写一次快照。写得太勤会拖垮 UI，ETA 估算也会抖。
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

    // 松开 Shift，免得它跨应用卡住——组合键打到一半被取消时就会这样。
    // 尽力而为，错误忽略。
    let _ = enigo.key(Key::Shift, Direction::Release);
    // 刻意不调 `into_inner()`：那会拆掉 ManuallyDrop 让 Enigo 真正的 Drop 跑起来，
    // 而它会累积 sleep 卡住数秒。ManuallyDrop 没有 Drop 实现，直接让它离开作用域
    // 就是包这一层的全部意义。
    result
}

/// 为 `ch` 发送一次按键。
///
/// `\n` / `\r` → Return，`\t` → Tab，其余都走 `text()`：它按字符输入，能处理任意
/// Unicode（含 CJK），不依赖键盘布局的键位表。
fn post_char(enigo: &mut Enigo, ch: char) -> Result<(), String> {
    match ch {
        '\n' | '\r' => enigo.key(Key::Return, Direction::Click),
        '\t' => enigo.key(Key::Tab, Direction::Click),
        _ => enigo.text(&ch.to_string()),
    }
    .map_err(|e| format!("{e}"))
}
