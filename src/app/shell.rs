//! App-lifetime state shared across windows and content modules.

use std::cell::Cell;
use std::sync::Arc;
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender};
use global_hotkey::GlobalHotKeyManager;
use gpui_kit::*;

use super::launcher::{LauncherAction, Module};
use crate::console_channel::{ConsoleChannel, ConsoleEntry};
use crate::runtime::run_service::RunService;

/// How often the global hotkey channel and launcher requests are polled.
pub(crate) const APP_TICK: Duration = Duration::from_millis(100);

/// App-lifetime state: the launcher window/channel, the registered content
/// modules, and the shared services (run service, console, LSP) that modules
/// reach for in their item actions. Window singletons live in each module's
/// own Global state, not here.
pub(crate) struct AppShell {
    /// The launcher window, while it is open. A stale handle means the window
    /// closed itself (Esc / blur / confirm).
    pub(crate) launcher: Option<AnyWindowHandle>,
    /// 取色浮窗组：每块显示器一个。再触发取色时整组先关再开。
    pub(crate) color_picker: Vec<AnyWindowHandle>,
    /// Sender handed to each launcher, which pushes the confirmed item's
    /// action here; the app tick drains and executes it.
    pub(crate) launcher_tx: Sender<LauncherAction>,
    /// The registered content modules, re-queried for items on every
    /// launcher open.
    pub(crate) modules: Vec<Box<dyn Module>>,
    /// Bumped when modules' item set changes while a launcher is open (e.g.
    /// app icons finished generating). The launcher re-collects items on its
    /// next paint instead of waiting for the next open.
    pub(crate) items_revision: Cell<u64>,
    pub(crate) run_service: Entity<RunService>,
    pub(crate) console_channel: ConsoleChannel,
    pub(crate) console_rx: Receiver<ConsoleEntry>,
    pub(crate) lsp: Result<Arc<crate::lsp::LspClient>, String>,
    /// Kept alive for the app lifetime; dropping unregisters the hotkey.
    pub(crate) _hotkey_manager: Option<GlobalHotKeyManager>,
    pub(crate) _app_tick: Task<()>,
}

impl Global for AppShell {}
