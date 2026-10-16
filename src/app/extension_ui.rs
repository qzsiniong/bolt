//! UI 扩展（gpui-shell）窗口。
//!
//! 一个 UI 扩展 = 一个原生窗口：入口脚本 `export default class extends View`，
//! 宿主在同进程 GPUI 主线程把它渲染成真实元素（没有 WebView）。窗口根就是
//! gpui-shell 的 `ShellRoot`（**不能**用会包一层 `base::Root` 的
//! `gpui_kit::open_window`），因此扩展脚本可以使用 shell 自带的
//! dialog / sheet / toast / 焦点与文本选择能力。
//!
//! 每次 `try_load` 之前都要重新设置 storage 与 capabilities：gpui-shell 的
//! 「默认策略」是进程级的，而 `try_load` 在载入那一刻把它快照进该窗口的
//! `Policy`，所以已经打开的窗口不会被后续设置改写。

use std::path::PathBuf;
use std::rc::Rc;

// 显式导入：`gpui_kit::*` 会把 gpui 的 `#[gpui::test]` 宏带进作用域，令下面
// `mod tests` 里的 `#[test]` 变成递归展开。
use gpui_kit::{
    App, AppContext as _, Bounds, Context, Entity, IntoElement, Render, TitlebarOptions, Window,
    WindowBounds, WindowOptions, px, size,
};

use super::shell::AppShell;
use crate::model::extension::{UiExtension, ensure_extension_data_dir, extension_data_dir};

/// UI 扩展窗口的默认尺寸。
const EXTENSION_WINDOW_SIZE: (f32, f32) = (880., 720.);
const EXTENSION_WINDOW_MIN_SIZE: (f32, f32) = (560., 420.);

/// 打开（或聚焦）一个 UI 扩展窗口。
pub(crate) fn open_extension_ui(cx: &mut App, ext: UiExtension) {
    let console = cx.global::<AppShell>().console_channel.clone();

    let Some(runtime) = cx.global::<AppShell>().shell_runtime.clone() else {
        console.error("UI 扩展运行时不可用（gpui-shell 初始化失败），无法打开扩展窗口");
        return;
    };

    // 单例：同一扩展只保留一个窗口；句柄失效说明窗口已经关了，重新开一个。
    let existing = cx
        .global_mut::<AppShell>()
        .extension_windows
        .get(&ext.root)
        .copied();
    if let Some(handle) = existing {
        if handle
            .update(cx, |_, window, _| window.activate_window())
            .is_ok()
        {
            cx.activate(true);
            return;
        }
        cx.global_mut::<AppShell>()
            .extension_windows
            .remove(&ext.root);
    }

    let id = ext.id().to_string();
    // 数据目录与授权必须在 load 之前设置：参见模块头部说明。
    let data_dir = match ensure_extension_data_dir(&id) {
        Ok(dir) => dir,
        Err(e) => {
            console.warn(format!("UI 扩展 {id} 的数据目录不可用：{e}"));
            extension_data_dir(&id)
        }
    };
    if let Err(e) = gpui_shell::set_bundle_id(&format!("scripter.{id}")) {
        console.warn(format!("UI 扩展 {id} 的应用标识不可用：{e}"));
    }
    gpui_shell::set_storage_path(data_dir.join("store.json"));
    gpui_shell::set_capabilities(
        ext.manifest
            .capabilities(&ext.root, &data_dir)
            .read_roots([ext.root.clone(), data_dir.clone()])
            .write_roots([data_dir])
            .exit(true),
    );

    // 扩展自己的 `svg("icons/x.svg")` 相对扩展根解析，需要先登记。
    cx.global::<AppShell>()
        .extension_assets
        .register_root(ext.root.clone());

    let root = ext.root.clone();
    let name = ext.name().to_string();
    let title = format!("{name} — Scripter");
    let load_console = console.clone();
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
            None,
            size(px(EXTENSION_WINDOW_SIZE.0), px(EXTENSION_WINDOW_SIZE.1)),
            cx,
        ))),
        window_min_size: Some(size(
            px(EXTENSION_WINDOW_MIN_SIZE.0),
            px(EXTENSION_WINDOW_MIN_SIZE.1),
        )),
        titlebar: Some(TitlebarOptions {
            title: Some(title.into()),
            ..Default::default()
        }),
        is_resizable: true,
        focus: true,
        ..Default::default()
    };

    // 非激活态开出的普通窗口会被压在别的应用后面，显式激活一次
    // （与 open_manager 同样的处理）。
    cx.activate(true);
    let opened = cx.open_window(options, move |window, cx| {
        load_shell_root(&runtime, &root, &load_console, window, cx)
    });
    let handle = match opened {
        Ok(handle) => handle,
        Err(e) => {
            console.error(format!("无法打开 UI 扩展窗口 {name}：{e}"));
            return;
        }
    };

    cx.global_mut::<AppShell>()
        .extension_windows
        .insert(ext.root.clone(), handle.into());
    console.info(format!("已打开 UI 扩展 {name}（{id} · {}）", ext.entry()));
}

/// 载入扩展并返回窗口根视图；加载失败时退回一个可选中复制的错误面，
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
                Err(e) => console.warn(format!("UI 扩展热重载不可用：{e:#}")),
            }
            shell_root
        }
        Err(e) => {
            let message = format!("{e:#}");
            console.error(format!(
                "UI 扩展加载失败 {}：{}",
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
            "扩展加载失败",
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

    /// JS 没有编译期检查：这里用 gpui-shell 自带的 `check` 在 headless 窗口里
    /// 真实加载 `ui-demo` 的入口脚本，构建并物化一次元素描述。入口脚本写错、
    /// 用了不存在的模块/组件/样式方法都会让这个测试失败。
    #[gpui_kit::test]
    fn demo_ui_extension_loads(cx: &mut gpui_kit::TestAppContext) {
        use crate::model::extension;

        let tmp = crate::test_support::TempDir::new("ui-demo-check");
        let extensions = tmp.0.join("extensions");
        std::fs::create_dir_all(&extensions).expect("create extensions dir");
        extension::ensure_demo_ui_extension(&extensions).expect("write demo extension");
        let dir = extensions.join(extension::DEMO_UI_ID);

        cx.update(gpui_shell::init);
        let runtime = gpui_shell::ShellRuntime::new_isolated().expect("start shell runtime");
        let window = cx.add_window(|_, _| Empty);
        let mut context = VisualTestContext::from_window(window.into(), cx);
        let checked = context.update(|window, cx| runtime.check(&dir, window, cx));
        assert!(
            checked.is_ok(),
            "ui-demo 加载失败：{}",
            checked.err().map(|e| format!("{e:#}")).unwrap_or_default()
        );
    }

    struct Empty;

    impl Render for Empty {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
        }
    }
}
