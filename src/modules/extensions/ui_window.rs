//! UI 命令（gpui-shell）窗口。
//!
//! 一条带 `ui` 块的命令 = 一个原生窗口：扩展目录下的 `main.js`
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
use super::actions::ActionSubject;
use super::exec;
use super::host::CommandTarget;
use super::manifest::{UiActionKind, UiBuiltin, ensure_extension_data_dir, extension_data_dir};
use super::ops;
use super::state::ExtensionsState;
use crate::app::assets::AssetIcon;
use crate::app::shell::AppShell;
use crate::ui::window::{WindowPin, WindowSpec, WindowTitleBar, button_with_tooltip, set_pinned};
use gpui_kit::assets::IconName;
use gpui_kit::component::Sizable as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, IntoElement, ParentElement as _, Render,
    SharedString, Styled as _, Window, div, px,
};

/// UI 命令窗口的最小尺寸；不会超过清单声明的尺寸。
const EXTENSION_WINDOW_MIN_SIZE: (f32, f32) = (560., 420.);

/// 标题栏右侧每个控件（动作按钮、图钉）占的宽度：`Button::small` 的图标按钮
/// 约 26pt + 4pt 间距（实测值，改按钮尺寸要跟着改）。
const CHROME_CONTROL_WIDTH: f32 = 30.;
/// 标题栏左侧的固定开销：macOS 红绿灯留白 80 + 标题块（标题 + 副标题）约
/// 92pt + 控件行内边距。
const CHROME_LEADING_WIDTH: f32 = 176.;

/// 宿主 chrome 需要的最小窗口宽度。
///
/// `TitleBar` 内部的 `#bar` 是 `flex_shrink_0`（gpui-component 里写死的），窄到
/// 放不下时它不会收缩，而是让右侧控件溢出窗口——图钉首当其冲（排最后）。所以
/// 宽度得由宿主兜底：动作按钮越多，窗口越宽。
fn chrome_min_width(actions: usize) -> f32 {
    // `+ 1` 是置顶图钉。
    CHROME_LEADING_WIDTH + (actions + 1) as f32 * CHROME_CONTROL_WIDTH
}

/// 扩展窗口的宿主外壳：宿主自绘标题栏（窗口标题 + 扩展名 + 自定义动作按钮 +
/// 置顶图钉）+ 脚本视图。
struct ExtensionChrome {
    shell_root: Entity<gpui_shell::ShellRoot>,
    /// 主标题：清单 `ui.title`，没写时回退到命令标题。
    title: SharedString,
    /// 副标题：扩展名，一眼看出这个窗口是谁开的。
    extension: SharedString,
    pin: Entity<WindowPin>,
    /// 清单 `ui.actions` 解析出的按钮，声明顺序即渲染顺序（图钉固定最右）。
    actions: Vec<ChromeAction>,
    /// builtin 动作与 command 动作都以它为操作对象（重载 / 开目录 / 跑命令）。
    subject: ActionSubject,
}

/// 标题栏右侧的一个动作按钮。图标与提示在开窗时解析好，渲染只负责画。
#[derive(Clone, Debug)]
struct ChromeAction {
    icon: AssetIcon,
    tooltip: SharedString,
    kind: ChromeKind,
}

/// 点击之后做什么。
#[derive(Clone, Debug)]
enum ChromeKind {
    Builtin(UiBuiltin),
    /// 点击执行该脚本命令（走扩展运行时；输出进 Console，无法回写本窗口 UI）。
    Command(Box<CommandTarget>),
}

/// 任意资产路径的图标：内置 Lucide 图标也是这条路（`icons/<name>.svg`），
/// 扩展自己提供的 `icons/x.svg` 由复合资产源按已登记的扩展根解析。
/// 实现挪到了 [`crate::app::assets::AssetIcon`]（Launcher 也要用）。
///
/// builtin 动作的默认图标与提示（清单没写 `icon` / `tooltip` 时用）。
fn builtin_presentation(builtin: UiBuiltin) -> (IconName, &'static str) {
    match builtin {
        UiBuiltin::Reload => (IconName::RefreshCw, "重新加载扩展"),
        UiBuiltin::OpenDataDir => (IconName::FolderOpen, "打开数据目录"),
        UiBuiltin::OpenExtensionDir => (IconName::Folder, "打开扩展目录"),
        UiBuiltin::CloseWindow => (IconName::X, "关闭窗口"),
    }
}

impl ExtensionChrome {
    /// 右侧动作按钮行；`None` = 清单没声明 actions，标题栏只有图钉。
    fn action_buttons(&self, cx: &Context<Self>) -> Option<AnyElement> {
        if self.actions.is_empty() {
            return None;
        }
        let mut row = div().flex().items_center().gap(px(4.)).flex_shrink_0();
        for (index, action) in self.actions.iter().enumerate() {
            row = row.child(button_with_tooltip(
                ("ext-ui-action-tooltip", index),
                Button::new(("ext-ui-action", index))
                    .icon(action.icon.clone())
                    .small()
                    .ghost()
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.run_action(index, window, cx);
                    })),
                action.tooltip.clone(),
            ));
        }
        Some(row.into_any_element())
    }

    /// 点击动作按钮的统一分发。builtin 复用扩展管理的 ops（本来就写 Console）；
    /// `close_window` 不写（窗口随即销毁，写了也看不见）。
    fn run_action(&self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        match &self.actions[index].kind {
            ChromeKind::Builtin(UiBuiltin::Reload) => ops::reload(cx, &self.subject),
            ChromeKind::Builtin(UiBuiltin::OpenDataDir) => ops::open_data_dir(cx, &self.subject),
            ChromeKind::Builtin(UiBuiltin::OpenExtensionDir) => ops::reveal(cx, &self.subject),
            ChromeKind::Builtin(UiBuiltin::CloseWindow) => window.remove_window(),
            ChromeKind::Command(target) => exec::run_command(cx, (**target).clone()),
        }
    }
}

impl Render for ExtensionChrome {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let actions_row = self.action_buttons(cx);
        let mut bar = WindowTitleBar::new(self.title.clone());
        // 扩展名当副标题；与主标题相同时不重复（脚手架模板默认就把它俩写成一样）。
        if self.extension != self.title {
            bar = bar.subtitle(self.extension.clone());
        }
        if let Some(row) = actions_row {
            bar = bar.trailing(row);
        }
        div()
            .flex()
            .flex_col()
            .size_full()
            // 宿主自绘标题栏：公共壳（标题 + 扩展名 + 自定义动作按钮 + 置顶图钉）。
            .child(bar.pin(self.pin.clone()))
            // flex_1：标题栏以下整块都是扩展的渲染区。渲染 `content()` 而不是
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

/// 关掉某个扩展的所有 UI 窗口，返回关掉的数量。
///
/// 重载 / 卸载前必须调：gpui-shell 的 watcher 与已加载视图都指向那个目录，
/// 目录被替换或移走之后它们只会一路报错，比关窗难排查得多。
pub(crate) fn close_extension_windows(cx: &mut App, root: &std::path::Path) -> usize {
    let handles: Vec<PathBuf> = {
        let state = cx.global::<ExtensionsState>();
        state
            .ui_windows
            .keys()
            .filter(|path| path.as_path() == root)
            .cloned()
            .collect()
    };
    let mut closed = 0;
    for path in handles {
        let Some(handle) = cx.global_mut::<ExtensionsState>().ui_windows.remove(&path) else {
            continue;
        };
        // 句柄失效说明窗口已经关了，不算这次关的。
        if handle
            .update(cx, |_, window, _| window.remove_window())
            .is_ok()
        {
            closed += 1;
        }
    }
    closed
}

/// 打开（或聚焦）一条 UI 命令的窗口。
pub(crate) fn open_extension_ui(cx: &mut App, target: CommandTarget) {
    let console = cx.global::<AppShell>().console_channel.clone();

    let Some(ui) = target.ui.clone() else {
        console.error(format!(
            "命令 {}.{} 没有 ui 声明",
            target.extension_id, target.command_id
        ));
        return;
    };

    let Some(runtime) = cx.global::<ExtensionsState>().shell_runtime.clone() else {
        console.error("UI 运行时不可用（gpui-shell 初始化失败），无法打开扩展窗口");
        return;
    };

    // 单例：同一扩展只保留一个窗口；句柄失效说明窗口已经关了，重新开一个。
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

    let extension_id = target.extension_id.clone();
    let pinned = ui.always_on_top;
    // 数据目录与授权必须在 load 之前设置：参见模块头部说明。
    let data_dir = match ensure_extension_data_dir(&extension_id) {
        Ok(dir) => dir,
        Err(e) => {
            console.warn(format!("扩展 {extension_id} 的数据目录不可用：{e}"));
            extension_data_dir(&extension_id)
        }
    };
    if let Err(e) = gpui_shell::set_bundle_id(&format!("bolt.{extension_id}")) {
        console.warn(format!("扩展 {extension_id} 的应用标识不可用：{e}"));
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

    // 扩展自己的 `svg("icons/x.svg")` 相对扩展根解析，需要先登记。
    cx.global::<ExtensionsState>()
        .assets
        .register_root(target.root.clone());

    let root = target.root.clone();
    let extension = target.extension_name.clone();
    // 窗口标题 = 清单的 `ui.title`（省略时回退到命令标题）。它同时是自绘标题栏
    // 的主标题与系统窗口标题——扩展名退到副标题，不再顶替窗口标题。
    let title = ui.display_title(&target.title).to_string();
    let load_console = console.clone();
    let window_size = (ui.width, ui.height);
    // 清单 ui.actions → 标题栏按钮。command 型按 (extension_id, command_id) 回
    // 宿主清单里查 target（校验已保证存在且是脚本命令；这里查不到就跳过并
    // 留 warn，不让一个坏按钮挡住整个窗口）。图标与提示在这里就解析好：
    // 清单写的覆盖默认值，`icon` 未写时用动作自带的 Lucide 图标。
    let chrome_actions: Vec<ChromeAction> = {
        let commands = cx.global::<ExtensionsState>().host.borrow().commands();
        let command_id = target.command_id.clone();
        ui.actions
            .iter()
            .filter_map(|action| {
                let (kind, default_icon, default_tooltip) = match action.kind() {
                    Ok(UiActionKind::Builtin(builtin)) => {
                        let (icon, tooltip) = builtin_presentation(builtin);
                        (
                            ChromeKind::Builtin(builtin),
                            icon.path(),
                            SharedString::from(tooltip),
                        )
                    }
                    Ok(UiActionKind::Command(action_command)) => {
                        let found = commands.iter().find(|candidate| {
                            candidate.extension_id == extension_id
                                && candidate.command_id == action_command
                        });
                        match found {
                            Some(candidate) => (
                                ChromeKind::Command(Box::new(candidate.clone())),
                                IconName::Play.path(),
                                SharedString::from(candidate.title.clone()),
                            ),
                            None => {
                                console.warn(format!(
                                    "命令 {extension_id}.{command_id} 的 ui.actions 引用了未知命令 `{action_command}`，已忽略"
                                ));
                                return None;
                            }
                        }
                    }
                    Err(error) => {
                        console.warn(format!(
                            "命令 {extension_id}.{command_id} 的 ui.actions 被忽略：{error}"
                        ));
                        return None;
                    }
                };
                let icon = action
                    .icon_path()
                    .map(SharedString::from)
                    .unwrap_or(default_icon);
                let tooltip = action
                    .tooltip_text()
                    .map(SharedString::from)
                    .unwrap_or(default_tooltip);
                Some(ChromeAction {
                    icon: AssetIcon(icon),
                    tooltip,
                    kind,
                })
            })
            .collect()
    };
    let subject = ActionSubject::from_target(&target);
    // 非激活态开出的普通窗口会被压在别的应用后面，显式激活一次。
    cx.activate(true);
    // 根视图必须是 ShellRoot（见模块头部说明），只能裸 `cx.open_window`——
    // `gpui_kit::open_window` 会包 base::Root，脚本侧的 tooltip/toast/dialog
    // 会静默失效。开窗参数仍统一走 `WindowSpec::to_options`。
    // 清单要的尺寸小于 chrome 需要的宽度时以 chrome 为准：否则标题栏右侧那排
    // 按钮（先是最右的图钉）会被裁掉。
    let min_size = (
        EXTENSION_WINDOW_MIN_SIZE
            .0
            .min(window_size.0)
            .max(chrome_min_width(chrome_actions.len())),
        EXTENSION_WINDOW_MIN_SIZE.1.min(window_size.1),
    );
    let size = (window_size.0.max(min_size.0), window_size.1.max(min_size.1));
    let spec = WindowSpec::new(title.clone())
        .size(size.0, size.1)
        .min_size(min_size.0, min_size.1)
        .pinned(pinned);
    let chrome_title: SharedString = title.clone().into();
    let chrome_extension: SharedString = extension.clone().into();
    let opened = cx.open_window(spec.to_options(cx), move |window, cx| {
        // 清单声明置顶：创建视图之前抬层级，`WindowPin` 首帧读到的就是真值。
        if pinned {
            set_pinned(window, true);
        }
        let shell_root = load_shell_root(&runtime, &root, &load_console, window, cx);
        let pin = cx.new(|_| WindowPin::new());
        let chrome = cx.new(|_| ExtensionChrome {
            shell_root,
            title: chrome_title,
            extension: chrome_extension,
            pin,
            actions: chrome_actions,
            subject,
        });
        cx.new(|cx| gpui_shell::ShellRoot::new(chrome.into(), window, cx))
    });
    let handle = match opened {
        Ok(handle) => handle,
        Err(e) => {
            console.error(format!("无法打开扩展窗口 {title}：{e}"));
            return;
        }
    };

    cx.global_mut::<ExtensionsState>()
        .ui_windows
        .insert(target.root.clone(), handle.into());
    console.info(format!(
        "已打开扩展窗口「{title}」（{extension_id} · {}{}）",
        target.command_id,
        if pinned { " · 置顶" } else { "" }
    ));
}

/// 载入扩展并返回窗口根里的脚本视图；加载失败时退回一个可选中复制的错误面，
/// 而不是让宿主 panic。
fn load_shell_root(
    runtime: &Rc<gpui_shell::ShellRuntime>,
    root: &PathBuf,
    console: &bolt_console::ConsoleChannel,
    window: &mut Window,
    cx: &mut App,
) -> Entity<gpui_shell::ShellRoot> {
    match runtime.try_load(root, window, cx) {
        Ok(shell_root) => {
            // debug 下保存即热重载；watcher 活到窗口结束。
            #[cfg(debug_assertions)]
            match runtime.watch(&shell_root, window, cx) {
                Ok(watcher) => watcher.forget(),
                Err(e) => console.warn(format!("扩展热重载不可用：{e:#}")),
            }
            shell_root
        }
        Err(e) => {
            let message = format!("{e:#}");
            console.error(format!(
                "扩展加载失败 {}：{}",
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

    #[test]
    fn chrome_min_width_covers_the_title_bar_controls() {
        use super::{CHROME_CONTROL_WIDTH, chrome_min_width};

        // 每多一个动作按钮，最小宽度就多一份控件宽度。
        assert_eq!(
            chrome_min_width(1) - chrome_min_width(0),
            CHROME_CONTROL_WIDTH
        );
        // 至少要放得下 macOS 红绿灯 + 标题 + 图钉。
        assert!(chrome_min_width(0) >= 200., "{}", chrome_min_width(0));
        // 5 个动作 + 图钉在清单声明的 280 宽窗口里放不下（实测被裁的正是图钉）。
        assert!(chrome_min_width(5) > 280.);
    }

    /// 在 headless 窗口里真实加载一个扩展目录的入口脚本，构建并物化一次元素。
    fn check_loads(dir: &std::path::Path, cx: &mut gpui_kit::TestAppContext) {
        cx.update(gpui_shell::init);
        // 与宿主同一套能力授予（见 open_extension_ui）：storage 固定给，剪贴板
        // 按清单（内置示例都开了）。否则示例里用 localStorage 会在 check 时
        // 报「storage is not granted」，而真实窗口里明明是好的。
        let storage = crate::test_support::TempDir::new("ext-ui-storage");
        gpui_shell::set_storage_path(storage.0.join("store.json"));
        gpui_shell::set_capabilities(
            gpui_shell::Capabilities::new()
                .read_roots([dir.to_path_buf(), storage.0.clone()])
                .write_roots([storage.0.clone()])
                .storage(true)
                .clipboard_read(true)
                .clipboard_write(true)
                .exit(true),
        );
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
    /// 真实加载内置示例扩展的 `main.js`，构建并物化一次元素描述。
    #[gpui_kit::test]
    fn demo_extension_ui_loads(cx: &mut gpui_kit::TestAppContext) {
        use super::super::manifest;

        let tmp = crate::test_support::TempDir::new("demo-ui-check");
        let extensions = tmp.0.join("extensions");
        std::fs::create_dir_all(&extensions).expect("create extensions dir");
        manifest::ensure_demo_extension(&extensions).expect("write demo extension");
        check_loads(&extensions.join(manifest::DEMO_DIR), cx);
    }

    /// 仓库内 `json-editor` 同样真实加载一次：入口脚本写错、引用了不存在的
    /// 模块/组件/样式方法都会让它失败。
    #[gpui_kit::test]
    fn json_editor_extension_ui_loads(cx: &mut gpui_kit::TestAppContext) {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("extensions/json-editor");
        assert!(dir.is_dir(), "缺少仓库扩展目录 {}", dir.display());
        check_loads(&dir, cx);
    }

    struct Empty;

    impl Render for Empty {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
        }
    }
}
