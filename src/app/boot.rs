//! Boot sequence: prepare the data directories, register the built-in host
//! modules, generate the script project files, start the language server, then
//! hand over to GPUI.

use std::sync::Arc;

use gpui_kit::*;

use super::assets::ExtensionAssets;
use super::shell::{APP_TICK, AppShell};
use super::shell_log;
use super::{hotkey, tray, windows};
use crate::console_channel::ConsoleChannel;
use crate::host::HostRegistry;
use crate::model::{project_gen, script_store};
use crate::modules::extensions::manifest as extension;
use crate::modules::extensions::state::ExtensionsState;
use crate::modules::json_editor::state::JsonEditorState;
use crate::modules::scripts::state::ScriptsState;
use crate::runtime::run_service::RunService;

pub(crate) fn run() {
    let scripts_dir = script_store::scripts_dir();
    if let Err(e) = script_store::ensure_dir(&scripts_dir) {
        eprintln!("ensure scripts dir: {e}");
    }
    // 扩展目录：debug 构建直接使用仓库里的 `extensions/`，改完重启即生效；
    // release 构建用数据目录，并把仓库扩展播种进去（只补缺失，不覆盖用户改动）。
    let extensions_dir = extension::active_extensions_dir();
    if let Err(e) = extension::ensure_dir(&extensions_dir) {
        eprintln!("ensure extensions dir: {e}");
    }
    if extension::uses_bundled_extensions() {
        // 仓库目录就是工作树：不往里写示例，也没有「同步」这回事。
        eprintln!("extensions dir: {}", extensions_dir.display());
    } else {
        // 首次启动给一个能直接跑的 UI 扩展示例（已存在则不动）。
        if let Err(e) = extension::ensure_demo_ui_extension(&extensions_dir) {
            eprintln!("ensure demo ui extension: {e}");
        }
        match extension::ensure_bundled_extensions(&extensions_dir) {
            Ok(n) if n > 0 => eprintln!("synced {n} bundled extension(s)"),
            Ok(_) => {}
            Err(e) => eprintln!("ensure bundled extensions: {e}"),
        }
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

    // UI 扩展的 `svg("icons/x.svg")` 相对扩展目录解析，而进程只有一个全局
    // AssetSource：用复合资源源在内置 Lucide 图标之前先查已打开的扩展目录。
    let assets = ExtensionAssets::new();
    let extension_assets = assets.clone();

    gpui_kit::application()
        .with_assets(assets)
        .run(move |cx| {
            gpui_kit::init(cx);

            // 托盘：Accessory 策略隐藏 Dock 图标（.app 的 LSUIElement 已覆盖
            // 打包场景，这里对齐 cargo run 行为）+ 菜单栏常驻图标。命令经
            // channel 回到下方 app tick 执行，ObjC 回调不直接碰 gpui。
            tray::set_accessory_policy();
            let tray_rx = tray::command_channel();
            tray::init();

            // One console channel for the whole app: the run service (and any
            // host side) pushes lines in; the manager window's Console panel
            // drains it while open. Output produced with no manager open stays
            // buffered and shows up when one opens.
            let (console_channel, console_rx) = ConsoleChannel::new();

            // UI 扩展（gpui-shell）的诊断全部走 tracing：脚本的 console 输出、
            // 加载失败、热重载结果。没有 subscriber 这些事件会被静默丢弃，
            // 所以必须在任何 shell 代码之前安装。
            shell_log::install(console_channel.clone());
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

            // Raycast-style global hotkey toggles the launcher; Cmd+Shift+T pins
            // the clipboard to a floating sticky.
            let (hotkey_manager, hotkey_statuses) = hotkey::register();
            for (id, result) in hotkey_statuses {
                match (id, result) {
                    (hotkey::HotKeyId::Launcher, Ok(_)) => {
                        console_channel.info("全局快捷键：Alt+Space 唤起启动器");
                    }
                    (hotkey::HotKeyId::PinClipboard, Ok(_)) => {
                        console_channel.info("全局快捷键：Cmd+Shift+T Pin 剪切板");
                    }
                    (_, Err(e)) => console_channel.error(e),
                }
            }

            // Launcher 里被确认的 item 闭包经这个 channel 回到 app tick 执行。
            let (launcher_tx, launcher_rx) = crossbeam_channel::unbounded();

            let app_tick = cx.spawn(async move |cx| loop {
                cx.background_executor().timer(APP_TICK).await;
                let fired = hotkey::poll_pressed();
                cx.update(|cx| {
                    for id in fired {
                        match id {
                            hotkey::HotKeyId::Launcher => windows::toggle_launcher(cx),
                            hotkey::HotKeyId::PinClipboard => {
                                crate::modules::pin::window::open_clipboard_pin(cx)
                            }
                        }
                    }
                    while let Ok(cmd) = tray_rx.try_recv() {
                        match cmd {
                            tray::TrayCommand::ToggleLauncher => windows::toggle_launcher(cx),
                            tray::TrayCommand::OpenManager => {
                                crate::modules::scripts::window::open_manager(cx, false)
                            }
                            tray::TrayCommand::NewScript => {
                                crate::modules::scripts::window::open_manager(cx, true)
                            }
                            tray::TrayCommand::Quit => cx.quit(),
                        }
                    }
                    windows::drain_launcher_requests(&launcher_rx, cx);
                });
            });

            // ── UI 扩展运行时（gpui-shell）─────────────────────────────────
            // 与普通脚本是两套运行时：扩展脚本在 GPUI 主线程执行、由宿主原生
            // 渲染。初始化失败只降级成「UI 扩展不可用」，脚本/编辑器链路不受
            // 影响（Launcher 里那条命令会给出 Console 报错）。
            gpui_shell::init(cx);
            let shell_runtime = match gpui_shell::ShellRuntime::new(cx) {
                Ok(runtime) => Some(runtime),
                Err(e) => {
                    console_channel.error(format!("UI 扩展运行时初始化失败：{e:#}"));
                    None
                }
            };

            // 扩展脚本的 `process.exit()` 只关掉它自己的窗口：Scripter 是常驻的
            // 热键应用，一个扩展不该结束整个进程。窗口句柄留在 ExtensionsState
            // 里会失效，下次打开命令时自动重建。
            let exit_console = console_channel.clone();
            gpui_shell::on_exit_request(move |request, window, _cx| {
                exit_console.info(format!(
                    "UI 扩展请求退出（code={}），仅关闭其窗口",
                    request.code()
                ));
                window.remove_window();
            });

            // 各内容模块的应用级状态：窗口单例与模块专属资源。
            cx.set_global(ScriptsState { manager: None });
            cx.set_global(JsonEditorState { window: None });
            cx.set_global(ExtensionsState {
                ui_windows: Default::default(),
                assets: extension_assets,
                shell_runtime,
            });

            let modules = crate::modules::default_modules(cx);
            console_channel.info(format!(
                "已加载模块：{}",
                modules
                    .iter()
                    .map(|m| m.name())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));

            cx.set_global(AppShell {
                launcher: None,
                launcher_tx,
                modules,
                items_revision: std::cell::Cell::new(0),
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
