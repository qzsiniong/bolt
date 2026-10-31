//! App 生命周期内跨窗口、跨内容模块共享的状态。

use std::cell::Cell;
use std::sync::Arc;
use std::time::Duration;

use crossbeam_channel::Sender;
use global_hotkey::GlobalHotKeyManager;
use gpui_kit::{AnyWindowHandle, Entity, Global, Task};

use super::launcher::{LauncherAction, Module};
use crate::runtime::run_service::RunService;
use bolt_console::ConsoleChannel;

/// 全局快捷键 channel 与 Launcher 请求的轮询频率。
pub(crate) const APP_TICK: Duration = Duration::from_millis(100);

/// App 生命周期状态：Launcher 窗口/channel、已注册的内容模块，以及模块在
/// 条目动作里要用的共享服务（run service、console、LSP）。各窗口的单例
/// 状态放在各自模块的 Global 里，不在这里。
pub(crate) struct AppShell {
    /// Launcher 窗口（开着的时候）。stale 的 handle 说明窗口自己关掉了
    /// （Esc / 失焦 / 确认）。
    pub(crate) launcher: Option<AnyWindowHandle>,
    /// 取色浮窗组：每块显示器一个。再触发取色时整组先关再开。
    pub(crate) color_picker: Vec<AnyWindowHandle>,
    /// 通知卡片窗口堆叠（index 0 贴屏幕底边距上方，越往后越靠下）；关闭时
    /// 其下方的窗口上移补位。见 ui/notify_window.rs。
    pub(crate) notify_windows: Vec<AnyWindowHandle>,
    /// 交给每个 Launcher 的发送端：确认条目后把它的动作推进这里，app tick
    /// 排空并执行。
    pub(crate) launcher_tx: Sender<LauncherAction>,
    /// 已注册的内容模块，每次打开 Launcher 时重新收集 items。
    pub(crate) modules: Vec<Box<dyn Module>>,
    /// Launcher 开着的时候模块的 item 集合变了（例如应用图标刚生成完）就
    /// 自增。Launcher 在下一次绘制重新收集 items，而不是等下次打开。
    pub(crate) items_revision: Cell<u64>,
    pub(crate) run_service: Entity<RunService>,
    /// 输出写入端。读者从 [`ConsoleChannel::history`] 取共享缓冲（脚本管理器
    /// 的 Console 面板、独立 Console 窗口各持一个游标）。
    pub(crate) console_channel: ConsoleChannel,
    pub(crate) lsp: Result<Arc<bolt_lsp::LspClient>, String>,
    /// 整个应用生命周期持有；drop 即注销快捷键。
    pub(crate) _hotkey_manager: Option<GlobalHotKeyManager>,
    pub(crate) _app_tick: Task<()>,
}

impl Global for AppShell {}
