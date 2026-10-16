//! Boot sequence: prepare the data directories, register the built-in host
//! modules, generate the script project files, start the language server, then
//! hand over to GPUI.

use std::sync::Arc;

use gpui_kit::*;

use super::assets::ExtensionAssets;
use super::shell::{APP_TICK, AppShell};
use super::shell_log;
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
    // 首次启动给一个能直接跑的 UI 扩展示例（已存在则不动）。
    if let Err(e) = extension::ensure_demo_ui_extension(&extensions_dir) {
        eprintln!("ensure demo ui extension: {e}");
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
            // 热键应用，一个扩展不该结束整个进程。窗口句柄留在
            // `extension_windows` 里会失效，下次打开命令时自动重建。
            let exit_console = console_channel.clone();
            gpui_shell::on_exit_request(move |request, window, _cx| {
                exit_console.info(format!(
                    "UI 扩展请求退出（code={}），仅关闭其窗口",
                    request.code()
                ));
                window.remove_window();
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
                shell_runtime,
                extension_windows: Default::default(),
                extension_assets,
            });

            // No window at startup: the app idles until Alt+Space.
        });
}
