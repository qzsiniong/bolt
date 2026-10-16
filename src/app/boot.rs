//! Boot sequence: prepare the data directories, register the built-in host
//! modules, generate the script project files, start the language server, then
//! hand over to GPUI.

use std::sync::Arc;

use gpui_kit::*;

use super::shell::{APP_TICK, AppShell};
use super::{hotkey, windows};
use crate::console_channel::ConsoleChannel;
use crate::host::HostRegistry;
use crate::model::{extension, project_gen, script_store};
use crate::runtime::run_service::RunService;

pub(crate) fn run() {
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
    crate::host::modules::register_all(&mut registry);
    if let Err(e) = project_gen::generate_project_files(&scripts_dir, &registry) {
        eprintln!("generate project files: {e}");
    }
    let registry = Arc::new(registry);

    // Spawn the language server before the UI so the editor gets diagnostics,
    // completions and hover from the start. Failure reason is surfaced in the
    // Console; the editor degrades to highlighting-only.
    let lsp_client = crate::lsp::LspClient::spawn(&scripts_dir).map(Arc::new);
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

            // Raycast-style global hotkey toggles the launcher.
            let (hotkey_manager, hotkey_status) = hotkey::register();
            match hotkey_status {
                Ok(()) => console_channel.info("全局快捷键：Alt+Space 唤起启动器"),
                Err(e) => console_channel.error(e),
            }

            // Launcher commands come back through this channel; the tick below
            // answers them in a plain app update.
            let (launcher_tx, launcher_rx) = crossbeam_channel::unbounded();

            let app_tick = cx.spawn(async move |cx| loop {
                cx.background_executor().timer(APP_TICK).await;
                let toggled = hotkey::poll_pressed();
                cx.update(|cx| {
                    if toggled {
                        windows::toggle_launcher(cx);
                    }
                    windows::drain_launcher_requests(&launcher_rx, cx);
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
