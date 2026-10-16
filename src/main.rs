mod command_model;
mod console_channel;
mod editor_ops;
mod extension;
mod host_module;
mod host_modules;
mod interaction;
mod interaction_window;
mod launcher;
mod lsp;
mod lsp_providers;
mod panels;
mod project_gen;
mod qr;
mod run_service;
mod runner;
mod script;
mod script_store;
mod ts_transform;
mod typer;
mod workspace;

use std::sync::Arc;
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender};
use global_hotkey::hotkey::{Code, HotKey, Modifiers};
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
use gpui_kit::*;

use crate::console_channel::{ConsoleChannel, ConsoleEntry};
use crate::host_module::HostRegistry;
use crate::launcher::{Launcher, LauncherRequest};
use crate::run_service::RunService;
use crate::script::Language;
use crate::workspace::Workspace;

/// How often the global hotkey channel and launcher requests are polled.
const APP_TICK: Duration = Duration::from_millis(100);

/// Launcher window size.
const LAUNCHER_SIZE: (f32, f32) = (720., 480.);

/// App-lifetime state: window singletons plus the pieces handed to the
/// manager window whenever it is (re)created.
struct AppShell {
    /// The launcher window, while it is open. A stale handle means the window
    /// closed itself (Esc / blur / confirm).
    launcher: Option<AnyWindowHandle>,
    /// Sender handed to each launcher, which pushes confirmed commands here.
    launcher_tx: Sender<LauncherRequest>,
    /// The manager window. The workspace is only weakly held: closing the
    /// window must drop it (and its console drain) so console output buffers
    /// in the channel until a manager is open again.
    manager: Option<(AnyWindowHandle, WeakEntity<Workspace>)>,
    run_service: Entity<RunService>,
    console_channel: ConsoleChannel,
    console_rx: Receiver<ConsoleEntry>,
    lsp: Result<Arc<lsp::LspClient>, String>,
    /// Kept alive for the app lifetime; dropping unregisters the hotkey.
    _hotkey_manager: Option<GlobalHotKeyManager>,
    _app_tick: Task<()>,
}

impl Global for AppShell {}

fn main() {
    let scripts_dir = script_store::scripts_dir();
    if let Err(e) = script_store::ensure_dir(&scripts_dir) {
        eprintln!("ensure scripts dir: {e}");
    }
    let extensions_dir = extension::extensions_dir();
    if let Err(e) = extension::ensure_dir(&extensions_dir) {
        eprintln!("ensure extensions dir: {e}");
    }

    // Register built-in host modules before generating project files so
    // their .d.ts declarations are available for IDE/LSP.
    let mut registry = HostRegistry::new();
    host_modules::register_all(&mut registry);
    if let Err(e) = project_gen::generate_project_files(&scripts_dir, &registry) {
        eprintln!("generate project files: {e}");
    }
    let registry = Arc::new(registry);

    // Spawn the language server before the UI so the editor gets diagnostics,
    // completions and hover from the start. Failure reason is surfaced in the
    // Console; the editor degrades to highlighting-only.
    let lsp_client = lsp::LspClient::spawn(&scripts_dir).map(Arc::new);
    if let Err(reason) = &lsp_client {
        eprintln!("LSP init failed: {reason}");
    }

    gpui_kit::application()
        // Register the embedded icon/SVG bundle (required for Icon rendering).
        // Use the full Lucide catalog: the default `Assets` bundle only embeds a
        // curated subset and silently drops icons outside it (e.g. Trash,
        // FileCode) — their SVG never loads, so the icon renders blank.
        .with_assets(gpui_kit::assets::AllAssets::new(""))
        .run(move |cx| {
            gpui_kit::init(cx);

            // One console channel for the whole app: the run service (and any
            // host side) pushes lines in; the manager window's Console panel
            // drains it while open. Output produced with no manager open stays
            // buffered and shows up when one opens.
            let (console_channel, console_rx) = ConsoleChannel::new();
            match &lsp_client {
                Ok(_) => {
                    console_channel.info("LSP: typescript-language-server connected");
                }
                Err(reason) => {
                    console_channel.warn(format!(
                        "LSP unavailable: {reason} — install with: npm i -g typescript-language-server typescript"
                    ));
                }
            }

            let run_service = cx.new(|_cx| {
                RunService::new(scripts_dir.clone(), registry.clone(), console_channel.clone())
            });

            // Raycast-style global hotkey toggles the launcher. The event
            // channel is crossbeam; poll it like the interaction drain does.
            let hotkey_manager = match GlobalHotKeyManager::new() {
                Ok(manager) => {
                    let hotkey = HotKey::new(Some(Modifiers::ALT), Code::Space);
                    match manager.register(hotkey) {
                        Ok(()) => {
                            console_channel.info("全局快捷键：Alt+Space 唤起启动器");
                            Some(manager)
                        }
                        Err(e) => {
                            console_channel.error(format!("注册全局快捷键失败：{e}"));
                            None
                        }
                    }
                }
                Err(e) => {
                    console_channel.error(format!("初始化全局快捷键失败：{e}"));
                    None
                }
            };
            // Launcher commands come back through this channel; the tick below
            // answers them in a plain app update.
            let (launcher_tx, launcher_rx) = crossbeam_channel::unbounded();

            let app_tick = cx.spawn(async move |cx| loop {
                cx.background_executor().timer(APP_TICK).await;
                let mut toggled = false;
                while let Ok(event) = GlobalHotKeyEvent::receiver().try_recv() {
                    if event.state == HotKeyState::Pressed {
                        toggled = true;
                    }
                }
                cx.update(|cx| {
                    if toggled {
                        toggle_launcher(cx);
                    }
                    // Dispatch here, not in the launcher's own confirm
                    // callback: that callback runs while the launcher window is
                    // being torn down, which drops the emitting view before a
                    // deferred entity event could reach it.
                    while let Ok(request) = launcher_rx.try_recv() {
                        match request {
                            LauncherRequest::OpenManager { new_script } => {
                                open_manager(cx, new_script)
                            }
                            LauncherRequest::RunFile(path) => run_path(cx, &path),
                        }
                    }
                });
            });

            cx.set_global(AppShell {
                launcher: None,
                launcher_tx,
                manager: None,
                run_service,
                console_channel,
                console_rx,
                lsp: lsp_client,
                _hotkey_manager: hotkey_manager,
                _app_tick: app_tick,
            });

            // No window at startup: the app idles until Alt+Space.
        });
}

/// Alt+Space: close the launcher if it is open, open it otherwise. A stale
/// handle means the window closed itself (Esc / blur / confirm) — reopen.
fn toggle_launcher(cx: &mut App) {
    let existing = cx.global_mut::<AppShell>().launcher.take();
    if let Some(handle) = existing {
        if handle
            .update(cx, |_, window, _| window.remove_window())
            .is_ok()
        {
            return;
        }
        // Stale handle: fall through and reopen.
    }

    let (requests, console) = {
        let shell = cx.global::<AppShell>();
        (shell.launcher_tx.clone(), shell.console_channel.clone())
    };
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::centered(
            size(px(LAUNCHER_SIZE.0), px(LAUNCHER_SIZE.1)),
            cx,
        )),
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
fn run_path(cx: &mut App, path: &std::path::Path) {
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
fn open_manager(cx: &mut App, new_script: bool) {
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
