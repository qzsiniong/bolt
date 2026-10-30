//! UI 命令（gpui-shell）窗口。
//!
//! 一条带 `ui` 块的命令 = 一个原生窗口：插件目录下的 `main.js`
//! `export default class extends View`，宿主在同进程 GPUI 主线程把它渲染成真实
//! 元素（没有 WebView）。
//!
//! **窗口的根视图必须是 gpui-shell 的 `ShellRoot`**：脚本侧的 tooltip、toast、
//! dialog、sheet 全靠 `Window::root::<ShellRoot>()` 去找宿主层（root.rs 的
//! `update` / `tooltip_overlay`），根视图不是 `ShellRoot` 时它们**静默失效**。
//! 因此宿主自绘标题栏（[`ExtensionChrome`]，右上角置顶图钉）只能作为 `ShellRoot`
//! 的 **content** 出现，不能反过来把 `ShellRoot` 包在外面。
//!
//! 需要保留 `try_load` 出来的那个 `ShellRoot`：热重载的 `runtime.watch` 要从
//! 它的 `application()` 元数据里取目录与入口，而脚本视图由它持有。所以
//! [`ExtensionChrome`] 留着它，只把它的 `content()` 渲染出来。
//!
//! 每次 `try_load` 之前都要重新设置 storage 与 capabilities：gpui-shell 的
//! 「默认策略」是进程级的，而 `try_load` 在载入那一刻把它快照进该窗口的
//! `Policy`，所以已经打开的窗口不会被后续设置改写。

use std::path::PathBuf;
use std::rc::Rc;

// 显式导入：`gpui_kit::*` 会把 gpui 的 `#[gpui::test]` 宏带进作用域，令下面
// `mod tests` 里的 `#[test]` 变成递归展开。
use super::host::CommandTarget;
use super::manifest::{ensure_extension_data_dir, extension_data_dir};
use super::state::ExtensionsState;
use crate::app::shell::AppShell;
use crate::ui::window_pin::{WindowPin, set_window_level};
use gpui_kit::component::{ActiveTheme as _, TitleBar};
use gpui_kit::{
    App, AppContext as _, Bounds, Context, Entity, FontWeight, IntoElement, ParentElement as _,
    Render, SharedString, Styled as _, TitlebarOptions, Window, WindowBounds, WindowKind,
    WindowOptions, div, px, size,
};

/// UI 命令窗口的最小尺寸；不会超过清单声明的尺寸。
const EXTENSION_WINDOW_MIN_SIZE: (f32, f32) = (560., 420.);

/// 扩展窗口的宿主外壳：宿主自绘标题栏（插件名 + 置顶图钉）+ 脚本视图。
struct ExtensionChrome {
    shell_root: Entity<gpui_shell::ShellRoot>,
    name: SharedString,
    pin: Entity<WindowPin>,
}

impl Render for ExtensionChrome {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        div()
            .flex()
            .flex_col()
            .size_full()
            .child(
                TitleBar::new()
                    .child(
                        div()
                            .flex_shrink_0()
                            .text_size(px(13.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.foreground)
                            .child(self.name.clone()),
                    )
                    // 右上角置顶图钉：公共控件自带按钮、状态与层级同步。
                    .child(self.pin.clone()),
            )
            // flex_1：标题栏以下整块都是插件的渲染区。渲染 `content()` 而不是
            // `shell_root` 本身：窗口根已经是外层 `ShellRoot`，脚本视图直接当它的
            // content 少一层；且每帧重新读取，热重载替换 content 后立刻生效。
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .child(self.shell_root.read(cx).content().clone()),
            )
    }
}

/// 打开（或聚焦）一条 UI 命令的窗口。
pub(crate) fn open_plugin_ui(cx: &mut App, target: CommandTarget) {
    let console = cx.global::<AppShell>().console_channel.clone();

    let Some(ui) = target.ui.clone() else {
        console.error(format!(
            "命令 {}.{} 没有 ui 声明",
            target.plugin_id, target.command_id
        ));
        return;
    };

    let Some(runtime) = cx.global::<ExtensionsState>().shell_runtime.clone() else {
        console.error("UI 运行时不可用（gpui-shell 初始化失败），无法打开插件窗口");
        return;
    };

    // 单例：同一插件只保留一个窗口；句柄失效说明窗口已经关了，重新开一个。
    let existing = cx
        .global::<ExtensionsState>()
        .ui_windows
        .get(&target.root)
        .cloned();
    if let Some(handle) = existing {
        if handle
            .update(cx, |_, window, _| window.activate_window())
            .is_ok()
        {
            cx.activate(true);
            return;
        }
        cx.global_mut::<ExtensionsState>()
            .ui_windows
            .remove(&target.root);
    }

    let plugin_id = target.plugin_id.clone();
    let pinned = ui.always_on_top;
    // 数据目录与授权必须在 load 之前设置：参见模块头部说明。
    let data_dir = match ensure_extension_data_dir(&plugin_id) {
        Ok(dir) => dir,
        Err(e) => {
            console.warn(format!("插件 {plugin_id} 的数据目录不可用：{e}"));
            extension_data_dir(&plugin_id)
        }
    };
    if let Err(e) = gpui_shell::set_bundle_id(&format!("bolt.{plugin_id}")) {
        console.warn(format!("插件 {plugin_id} 的应用标识不可用：{e}"));
    }
    // 目录里没有 gpui-shell.json：能力完全由宿主安装，清单里的开关决定剪贴板。
    let clipboard = ui.capabilities.clipboard;
    gpui_shell::set_storage_path(data_dir.join("store.json"));
    gpui_shell::set_capabilities(
        gpui_shell::Capabilities::new()
            .read_roots([target.root.clone(), data_dir.clone()])
            .write_roots([data_dir])
            .storage(true)
            .clipboard_read(clipboard)
            .clipboard_write(clipboard)
            .exit(true),
    );

    // 插件自己的 `svg("icons/x.svg")` 相对插件根解析，需要先登记。
    cx.global::<ExtensionsState>()
        .assets
        .register_root(target.root.clone());

    let root = target.root.clone();
    let name = target.plugin_name.clone();
    let title = format!("{name} — Bolt");
    let load_console = console.clone();
    let window_size = (ui.width, ui.height);
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
            None,
            size(px(window_size.0), px(window_size.1)),
            cx,
        ))),
        // 统一开普通窗口：置顶用 `set_window_level` 直接改层级（PopUp 是
        // 非激活面板，形态上与普通窗口结构性不同，不适合来回切换）。
        kind: WindowKind::Normal,
        window_min_size: Some(size(
            px(EXTENSION_WINDOW_MIN_SIZE.0.min(window_size.0)),
            px(EXTENSION_WINDOW_MIN_SIZE.1.min(window_size.1)),
        )),
        titlebar: Some(TitlebarOptions {
            title: Some(title.into()),
            ..TitleBar::title_bar_options()
        }),
        is_resizable: true,
        focus: true,
        // `TitleBar::window_options()` 一并给出「自绘标题栏自己处理拖动与双击、
        // AppKit 不再把它当系统拖动区」的开窗参数。
        ..TitleBar::window_options()
    };

    // 非激活态开出的普通窗口会被压在别的应用后面，显式激活一次。
    cx.activate(true);
    let chrome_name: SharedString = name.clone().into();
    let opened = cx.open_window(options, move |window, cx| {
        let shell_root = load_shell_root(&runtime, &root, &load_console, window, cx);
        let pin = cx.new(|_| WindowPin::new(pinned));
        let chrome = cx.new(|_| ExtensionChrome {
            shell_root,
            name: chrome_name,
            pin,
        });
        // 根视图必须是 ShellRoot（见模块头部说明），chrome 只能作为它的 content。
        cx.new(|cx| gpui_shell::ShellRoot::new(chrome.into(), window, cx))
    });
    let handle = match opened {
        Ok(handle) => handle,
        Err(e) => {
            console.error(format!("无法打开插件窗口 {name}：{e}"));
            return;
        }
    };

    // 清单声明置顶：开窗即上浮到 floating 层级。
    if pinned {
        let _ = handle.update(cx, |_, window, _| set_window_level(window, true));
    }

    cx.global_mut::<ExtensionsState>()
        .ui_windows
        .insert(target.root.clone(), handle.into());
    console.info(format!(
        "已打开插件窗口 {name}（{plugin_id} · {}{}）",
        target.command_id,
        if pinned { " · 置顶" } else { "" }
    ));
}

/// 载入插件并返回窗口根里的脚本视图；加载失败时退回一个可选中复制的错误面，
/// 而不是让宿主 panic。
fn load_shell_root(
    runtime: &Rc<gpui_shell::ShellRuntime>,
    root: &PathBuf,
    console: &crate::console_channel::ConsoleChannel,
    window: &mut Window,
    cx: &mut App,
) -> Entity<gpui_shell::ShellRoot> {
    match runtime.try_load(root, window, cx) {
        Ok(shell_root) => {
            // debug 下保存即热重载；watcher 活到窗口结束。
            #[cfg(debug_assertions)]
            match runtime.watch(&shell_root, window, cx) {
                Ok(watcher) => watcher.forget(),
                Err(e) => console.warn(format!("插件热重载不可用：{e:#}")),
            }
            shell_root
        }
        Err(e) => {
            let message = format!("{e:#}");
            console.error(format!(
                "插件加载失败 {}：{}",
                root.display(),
                first_lines(&message, 12)
            ));
            let content = cx.new(|_| LoadFailure(message));
            cx.new(|cx| gpui_shell::ShellRoot::new(content.into(), window, cx))
        }
    }
}

/// 加载失败时窗口里显示的内容。
struct LoadFailure(String);

impl Render for LoadFailure {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        gpui_shell::failure_surface(
            "插件加载失败",
            &self.0,
            "修正入口脚本后重新打开该命令",
            window,
            cx,
        )
    }
}

/// 只保留开头若干行：Console 一行输出不适合塞进整段堆栈，
/// 窗口错误面里仍是完整文本。
fn first_lines(text: &str, limit: usize) -> String {
    let mut lines = text.lines();
    let head: Vec<&str> = lines.by_ref().take(limit).collect();
    let mut out = head.join("\n");
    if lines.next().is_some() {
        out.push_str("\n…");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::first_lines;
    use gpui_kit::{Context, IntoElement, Render, VisualTestContext, Window, div};

    #[test]
    fn first_lines_truncates_and_marks() {
        assert_eq!(first_lines("a\nb", 3), "a\nb");
        assert_eq!(first_lines("a\nb\nc", 2), "a\nb\n…");
    }

    /// 在 headless 窗口里真实加载一个插件目录的入口脚本，构建并物化一次元素。
    fn check_loads(dir: &std::path::Path, cx: &mut gpui_kit::TestAppContext) {
        cx.update(gpui_shell::init);
        let runtime = gpui_shell::ShellRuntime::new_isolated().expect("start shell runtime");
        let window = cx.add_window(|_, _| Empty);
        let mut context = VisualTestContext::from_window(window.into(), cx);
        let checked = context.update(|window, cx| runtime.check(dir, window, cx));
        assert!(
            checked.is_ok(),
            "{} 加载失败：{}",
            dir.display(),
            checked.err().map(|e| format!("{e:#}")).unwrap_or_default()
        );
    }

    /// JS 没有编译期检查：这里用 gpui-shell 自带的 `check` 在 headless 窗口里
    /// 真实加载内置示例插件的 `main.js`，构建并物化一次元素描述。
    #[gpui_kit::test]
    fn demo_plugin_ui_loads(cx: &mut gpui_kit::TestAppContext) {
        use super::super::manifest;

        let tmp = crate::test_support::TempDir::new("demo-ui-check");
        let plugins = tmp.0.join("plugins");
        std::fs::create_dir_all(&plugins).expect("create plugins dir");
        manifest::ensure_demo_plugin(&plugins).expect("write demo plugin");
        check_loads(&plugins.join(manifest::DEMO_DIR), cx);
    }

    /// 仓库内 `json-editor` 同样真实加载一次：入口脚本写错、引用了不存在的
    /// 模块/组件/样式方法都会让它失败。
    #[gpui_kit::test]
    fn json_editor_plugin_ui_loads(cx: &mut gpui_kit::TestAppContext) {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("extensions/plugins/json-editor-v1.0.0.js");
        assert!(dir.is_dir(), "缺少仓库插件目录 {}", dir.display());
        check_loads(&dir, cx);
    }

    struct Empty;

    impl Render for Empty {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
        }
    }
}
