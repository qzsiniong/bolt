//! Boot sequence: prepare the data directories, register the built-in host
//! modules, generate the script project files, start the language server, then
//! hand over to GPUI.

use std::sync::Arc;

use gpui_kit::AppContext;

use super::assets::ExtensionAssets;
use super::shell::{APP_TICK, AppShell};
use super::shell_log;
use super::{hotkey, tray, usage, windows};
use crate::console_channel::ConsoleChannel;
use crate::host::HostRegistry;
use crate::model::{project_gen, script_store};
use crate::modules::extensions::host::ExtensionsHost;
use crate::modules::extensions::js_env::BoltJsEnv;
use crate::modules::extensions::manifest as extension;
use crate::modules::extensions::state::ExtensionsState;
use crate::modules::json_editor::state::JsonEditorState;
use crate::modules::scripts::state::ScriptsState;
use crate::runtime::run_service::RunService;

pub(crate) fn run() {
    // Console channel 先于一切创建：启动期错误（目录准备、播种、项目文件）
    // 也进 Console 面板——打包成 .app 后 stderr 用户根本看不到。channel 有界
    // （丢最旧），面板打开前产生的条目会缓存，打开后一并显示。Error 级条目
    // 同时落盘 bolt.log：Console 面板只在脚本管理器窗口里，用户若从不打开
    // 它，落盘文件就是启动失败唯一的出路。
    let log_path = dirs::data_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("bolt")
        .join("bolt.log");
    let (console_channel, console_rx) = ConsoleChannel::new_with_error_log(log_path);

    let scripts_dir = script_store::scripts_dir();
    if let Err(e) = script_store::ensure_dir(&scripts_dir) {
        console_channel.error(format!("创建脚本目录失败：{e}"));
    }
    // 插件根：`extensions/`。debug 构建直接用仓库目录，改完重启即生效；
    // release 构建用数据目录，并把仓库插件播种进去（只补缺失，不覆盖用户改动）。
    let extensions_dir = extension::active_extensions_dir();
    if let Err(e) = extension::ensure_dir(&extensions_dir) {
        console_channel.error(format!("创建扩展目录失败：{e}"));
    }
    if extension::uses_bundled_extensions() {
        // 仓库目录就是工作树：不往里写示例，也没有「同步」这回事。
        console_channel.info(format!("扩展目录：{}", extensions_dir.display()));
    } else {
        // 首次启动给一个能直接跑的示例插件（已存在则不动）。
        if let Err(e) = extension::ensure_demo_plugin(&extensions_dir) {
            console_channel.warn(format!("生成示例插件失败：{e}"));
        }
        match extension::ensure_bundled_extensions(&extensions_dir) {
            Ok(n) if n > 0 => console_channel.info(format!("已同步 {n} 个仓库插件")),
            Ok(_) => {}
            Err(e) => console_channel.warn(format!("同步仓库插件失败：{e}")),
        }
    }

    // Register built-in host modules before generating project files so
    // their .d.ts declarations are available for IDE/LSP.
    let mut registry = HostRegistry::new();
    crate::host::modules::register_all(&mut registry);
    if let Err(e) = project_gen::generate_project_files(&scripts_dir, &registry) {
        console_channel.warn(format!("生成项目文件失败：{e}"));
    }
    let registry = Arc::new(registry);

    // 启动时裁剪一次使用记录：删掉超过保留期的老事件，避免文件无限增长
    // （`record` 只裁被记录的那一个 item）。
    usage::prune_usage();

    // Spawn the language server before the UI so the editor gets diagnostics,
    // completions and hover from the start. Failure reason is surfaced in the
    // Console below; the editor degrades to highlighting-only.
    let lsp_client = crate::lsp::LspClient::spawn(&scripts_dir).map(Arc::new);

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
            tray::set_accessory_policy(&console_channel);
            let tray_rx = tray::command_channel();
            tray::init(&console_channel);

            // UI 命令（gpui-shell）的诊断全部走 tracing：脚本的 console 输出、
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

            // plux 插件宿主：Loader 管生命周期与元数据，PluginStore 管执行。
            // 必须在 RunService 之前就位——命令要在工作线程里跑，RunService 需要
            // 拿到 store 才能投递。
            let js_env = Arc::new(BoltJsEnv {
                registry: registry.clone(),
                console: console_channel.clone(),
            });
            let extensions_host = ExtensionsHost::new(
                &extensions_dir,
                js_env,
                &crate::modules::extensions::prefs::path(),
            );
            let plugin_store = extensions_host.store();

            let run_service = cx.new(|_cx| {
                RunService::new(
                    scripts_dir.clone(),
                    registry.clone(),
                    console_channel.clone(),
                    plugin_store.clone(),
                )
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

            // 通知卡片：mac_ffi::notify 入队，app tick drain 开窗。
            crate::ui::notify_window::init_channel();

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
                    crate::ui::notify_window::drain_notifications(cx);
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

            // 扩展脚本的 `process.exit()` 只关掉它自己的窗口：Bolt 是常驻的
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
                host: std::cell::RefCell::new(extensions_host),
                manager: None,
                create: None,
                registry: registry.clone(),
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
                color_picker: Vec::new(),
                notify_windows: Vec::new(),
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
