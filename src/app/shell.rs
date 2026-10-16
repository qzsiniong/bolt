//! App-lifetime state shared across windows.

use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender};
use global_hotkey::GlobalHotKeyManager;
use gpui_kit::*;

use super::assets::ExtensionAssets;
use crate::console_channel::{ConsoleChannel, ConsoleEntry};
use crate::runtime::run_service::RunService;
use crate::ui::launcher::LauncherRequest;
use crate::ui::workspace::Workspace;

/// How often the global hotkey channel and launcher requests are polled.
pub(super) const APP_TICK: Duration = Duration::from_millis(100);

/// App-lifetime state: window singletons plus the pieces handed to the
/// manager window whenever it is (re)created.
pub(super) struct AppShell {
    /// The launcher window, while it is open. A stale handle means the window
    /// closed itself (Esc / blur / confirm).
    pub(super) launcher: Option<AnyWindowHandle>,
    /// Sender handed to each launcher, which pushes confirmed commands here.
    pub(super) launcher_tx: Sender<LauncherRequest>,
    /// The manager window. The workspace is only weakly held: closing the
    /// window must drop it (and its console drain) so console output buffers
    /// in the channel until a manager is open again.
    pub(super) manager: Option<(AnyWindowHandle, WeakEntity<Workspace>)>,
    pub(super) run_service: Entity<RunService>,
    pub(super) console_channel: ConsoleChannel,
    pub(super) console_rx: Receiver<ConsoleEntry>,
    pub(super) lsp: Result<Arc<crate::lsp::LspClient>, String>,
    /// Kept alive for the app lifetime; dropping unregisters the hotkey.
    pub(super) _hotkey_manager: Option<GlobalHotKeyManager>,
    pub(super) _app_tick: Task<()>,
    /// The gpui-shell runtime UI extensions run in. `None` when it could not be
    /// created; opening a UI extension then reports the reason instead of
    /// failing the whole app.
    pub(super) shell_runtime: Option<Rc<gpui_shell::ShellRuntime>>,
    /// One window per UI extension, keyed by its directory. A stale handle
    /// means the window closed itself (user closed it / the script asked to
    /// exit) and the next open creates a fresh one.
    pub(super) extension_windows: HashMap<PathBuf, AnyWindowHandle>,
    /// Handle to the installed asset source, so an extension's own `svg()`
    /// paths can be registered before its window loads.
    pub(super) extension_assets: ExtensionAssets,
}

impl Global for AppShell {}
