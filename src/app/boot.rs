//! 启动序列：准备数据目录、注册内置宿主模块、生成脚手架工程文件、
//! 启动语言服务，然后交给 GPUI。

use std::sync::Arc;

use gpui_kit::AppContext;

use super::assets::ExtensionAssets;
use super::shell::{APP_TICK, AppShell};
use super::shell_log;
use super::{hotkey, tray, usage, windows};
use crate::host::HostRegistry;
use crate::model::project_gen;
use crate::modules::extensions::host::ExtensionsHost;
use crate::modules::extensions::js_env::BoltJsEnv;
use crate::modules::extensions::manifest as extension;
use crate::modules::extensions::state::ExtensionsState;
use crate::modules::json_editor::state::JsonEditorState;
use crate::modules::scripts::state::ScriptsState;
use crate::runtime::run_service::RunService;
use bolt_console::ConsoleChannel;
use bolt_model::script_store;

pub(crate) fn run() {
    // Console 先于一切创建：启动期错误（目录准备、播种、项目文件）也要进
    // Console——打包成 .app 后 stderr 用户根本看不到。缓冲有界（丢最旧，留最近
    // 1000 条），视图打开前产生的条目会留着，打开后一并显示。Error 级条目
    // 同时落盘 bolt.log：用户若从不打开 Console 窗口，落盘文件就是启动失败的
    // 唯一线索。调试构建下每条输出还会同时打到终端（见 console_channel）。
    let log_path = bolt_model::paths::data_root().join("bolt.log");
    let console_channel = ConsoleChannel::new_with_error_log(log_path);

    let scripts_dir = script_store::scripts_dir();
    if let Err(e) = script_store::ensure_dir(&scripts_dir) {
        console_channel.error(format!("创建脚本目录失败：{e}"));
    }
    // 扩展根：`extensions/`。debug 构建直接用仓库目录，改完重启即生效；
    // release 构建用数据目录，并把仓库扩展播种进去（只补缺失，不覆盖用户改动）。
    let extensions_dir = extension::active_extensions_dir();
    if let Err(e) = extension::ensure_dir(&extensions_dir) {
        console_channel.error(format!("创建扩展目录失败：{e}"));
    }
    // 清单的 JSON Schema 摆在扩展根：扩展的 extension.json 用 `$schema:
    // "../extension.schema.json"` 就能拿到编辑器的补全与校验。
    if let Err(e) = extension::ensure_schema(&extensions_dir) {
        console_channel.warn(format!("写入 {} 失败：{e}", extension::SCHEMA_FILE));
    }
    if extension::uses_bundled_extensions() {
        // 仓库目录就是工作树：不往里写示例，也没有「同步」这回事。
        console_channel.info(format!("扩展目录：{}", extensions_dir.display()));
    } else {
        // 首次启动给一个能直接跑的示例扩展（已存在则不动）。
        if let Err(e) = extension::ensure_demo_extension(&extensions_dir) {
            console_channel.warn(format!("生成示例扩展失败：{e}"));
        }
        match extension::ensure_bundled_extensions(&extensions_dir) {
            Ok(n) if n > 0 => console_channel.info(format!("已同步 {n} 个仓库扩展")),
            Ok(_) => {}
            Err(e) => console_channel.warn(format!("同步仓库扩展失败：{e}")),
        }
    }

    // 先注册内置宿主模块再生成工程文件，这样它们的 .d.ts 声明对
    // IDE/LSP 可见。
    let mut registry = HostRegistry::new();
    crate::host::modules::register_all(&mut registry);
    if let Err(e) = project_gen::generate_project_files(&scripts_dir, &registry) {
        console_channel.warn(format!("生成项目文件失败：{e}"));
    }
    let registry = Arc::new(registry);

    // 启动时裁剪一次使用记录：删掉超过保留期的老事件，避免文件无限增长
    // （`record` 只裁被记录的那一个 item）。
    usage::prune_usage();

    // 语言服务先于 UI 启动，编辑器从第一帧起就有诊断、补全和悬停。
    // 失败原因会打进下面的 Console；编辑器退化成纯语法高亮。
    let lsp_client = bolt_lsp::LspClient::spawn(&scripts_dir).map(Arc::new);

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

            // plux 扩展宿主：Loader 管生命周期与元数据，ExtensionStore 管执行。
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
            let extension_store = extensions_host.store();

            let run_service = cx.new(|_cx| {
                RunService::new(
                    scripts_dir.clone(),
                    registry.clone(),
                    console_channel.clone(),
                    extension_store.clone(),
                )
            });

            // Raycast 风格的全局快捷键唤起/收起 launcher；Cmd+Shift+T 把剪贴板
            // 钉成浮动便签。
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
            cx.set_global(crate::modules::console::ConsoleState { window: None });
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
                lsp: lsp_client,
                _hotkey_manager: hotkey_manager,
                _app_tick: app_tick,
            });

            // 启动时不开任何窗口：应用闲置，等 Alt+Space。
        });
}
