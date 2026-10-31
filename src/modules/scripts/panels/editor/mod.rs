//! 编辑器面板：承载代码编辑器、它的 LSP providers，以及当前载入的脚本。
//!
//! 文档层面的管道（载入 / 保存 / 运行 / 诊断）在 `document`，编辑快捷键
//! 在 `actions`。所有字段都留在本模块且私有：子模块可直接读写，拆分
//! 不需要额外的 accessor 间接层。

use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use gpui_kit::base::dock::PanelEvent;
use gpui_kit::component::ActiveTheme;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::dock::{BasePanel, Panel};
use gpui_kit::component::input::{Editor, EditorState, InputEvent, TabSize};
use gpui_kit::component::{Disableable, IconName};
use gpui_kit::{
    AnyElement, App, AppContext, Context, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement, IntoElement, KeyBinding, ParentElement, Render, Styled, Subscription, Task,
    Window, div, px,
};
use parking_lot::Mutex;

use self::actions::{
    DeleteLine, DuplicateLineDown, DuplicateLineUp, MoveLineDown, MoveLineUp, ToggleBlockComment,
    ToggleComment,
};
use crate::lsp::LspClient;
use crate::lsp::providers::{DocHandle, LspBridge};
use crate::model::script::{Language, Script};
use crate::model::script_store;

mod actions;
mod document;

/// 用户点击 Run 时发出。
#[derive(Clone, Debug)]
pub struct RunRequested {
    /// 脚本文本内容。
    pub content: String,
    /// 脚本语言。
    pub language: Language,
    /// 文件名（用于运行时展示与日志）。
    pub file_name: String,
}

/// 用户点击 Stop 时发出，请求宿主中止正在运行的脚本。
#[derive(Clone, Debug)]
pub struct StopRequested;

/// 编辑器面板：代码编辑器 + 工具栏（运行/停止/保存）+ 未打开脚本时的
/// 快捷键提示页。
pub struct EditorPanel {
    focus_handle: FocusHandle,
    scripts_dir: PathBuf,
    editor: Entity<EditorState>,
    current_script: Option<Script>,
    dirty: bool,
    /// 脚本正由本面板启动执行中；驱动 Run/Stop 的禁用态。归宿主
    /// （Workspace）所有，由它调 `set_running`。
    running: bool,
    pending_load: Option<Script>,
    lsp: Option<Arc<LspClient>>,
    doc: Arc<Mutex<Option<DocHandle>>>,
    _diagnostics_task: Option<Task<()>>,
    _subscription: Subscription,
}

impl EditorPanel {
    /// 创建编辑器面板：绑定编辑快捷键，LSP 可用时注册 providers。
    pub fn new(window: &mut Window, cx: &mut Context<Self>, lsp: Option<Arc<LspClient>>) -> Self {
        // 编辑快捷键。`Some("Input")` 匹配编辑器的按键上下文
        // （"Input mode=editor"），因此只在编辑器聚焦时触发，不会遮蔽
        // 应用其他位置的无关绑定。注意 `cmd-shift-/` 在 macOS 上按不出来
        // （shift 后的标点键上报为 `cmd-?`），所以块注释用 `cmd-alt-/`。
        cx.bind_keys([
            KeyBinding::new("cmd-/", ToggleComment, Some("Input")),
            KeyBinding::new("cmd-alt-/", ToggleBlockComment, Some("Input")),
            KeyBinding::new("cmd-shift-k", DeleteLine, Some("Input")),
            KeyBinding::new("shift-alt-up", DuplicateLineUp, Some("Input")),
            KeyBinding::new("shift-alt-down", DuplicateLineDown, Some("Input")),
            KeyBinding::new("alt-up", MoveLineUp, Some("Input")),
            KeyBinding::new("alt-down", MoveLineDown, Some("Input")),
        ]);

        let editor = cx.new(|ecx| {
            EditorState::new(window, ecx)
                .language("typescript")
                .line_number(true)
                .folding(true)
                .show_whitespaces(false)
                .tab_size(TabSize {
                    tab_size: 2,
                    hard_tabs: false,
                })
                .placeholder("Select a script from the list…")
        });

        // 语言服务器可用时，在编辑器上注册 LSP providers。
        let doc: Arc<Mutex<Option<DocHandle>>> = Arc::new(Mutex::new(None));
        if let Some(client) = lsp.clone() {
            let bridge = Rc::new(LspBridge::new(Some(client), doc.clone()));
            editor.update(cx, |state, _| {
                let lsp = state.lsp_mut();
                lsp.completion_provider = Some(bridge.clone());
                lsp.hover_provider = Some(bridge.clone());
                lsp.definition_provider = Some(bridge.clone());
                lsp.code_action_providers.push(bridge);
            });
        }

        let subscription = cx.subscribe(&editor, |this, _state, event: &InputEvent, cx| {
            if let InputEvent::Change = event {
                // dirty 的含义是编辑器内容与载入脚本已保存的内容不一致。
                // 比较内容（而不是把每次 Change 都当成用户编辑），可避免
                // 载入脚本时 `set_value` 把编辑器误标为 dirty。
                let value = this.editor.read(cx).value().to_string();
                this.dirty = this
                    .current_script
                    .as_ref()
                    .map(|s| s.content != value)
                    .unwrap_or(false);
                this.sync_did_change(cx);
                cx.notify();
            }
        });

        let diagnostics_task = if lsp.is_some() {
            Some(cx.spawn(Self::diagnostics_loop))
        } else {
            None
        };

        Self {
            focus_handle: cx.focus_handle(),
            scripts_dir: script_store::scripts_dir(),
            editor,
            current_script: None,
            dirty: false,
            running: false,
            pending_load: None,
            lsp,
            doc,
            _diagnostics_task: diagnostics_task,
            _subscription: subscription,
        }
    }
}

impl BasePanel for EditorPanel {
    fn panel_name(&self) -> &'static str {
        "Editor"
    }
}

impl Panel for EditorPanel {
    fn title(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let label = self
            .current_script
            .as_ref()
            .map(|s| s.file_name.clone())
            .unwrap_or_else(|| "Editor".into());
        div().text_sm().child(label).into_any_element()
    }
}

impl EventEmitter<PanelEvent> for EditorPanel {}
impl EventEmitter<RunRequested> for EditorPanel {}
impl EventEmitter<StopRequested> for EditorPanel {}

impl Focusable for EditorPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for EditorPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(script) = self.pending_load.take() {
            self.load_script(&script, window, cx);
        }
        let can_run = self.has_runnable();
        let can_save = self.dirty && self.current_script.is_some();
        let title = self
            .current_script
            .as_ref()
            .map(|s| s.file_name.clone())
            .unwrap_or_else(|| "No script selected".to_string());
        let dirty = self.dirty;

        let theme = cx.theme();
        let border = theme.border;
        let muted_fg = theme.muted_foreground;
        let fg = theme.foreground;
        let warning = theme.warning;

        let toolbar = div()
            .flex()
            .items_center()
            .gap_2()
            .px_3()
            .py_2()
            .border_b_1()
            .border_color(border)
            .child(
                Button::new("run")
                    .icon(IconName::Play)
                    .label("Run")
                    .primary()
                    .compact()
                    .disabled(!can_run || self.running)
                    .tooltip("Run script (⌘⏎)")
                    .on_click(cx.listener(|this, _, window, cx| this.run_current(window, cx))),
            )
            .child(
                // Stop 用完整的 Lucide 目录（assets::IconName）；组件层的
                // 枚举只带固定的兼容子集，没有 SquareStop 变体。
                Button::new("stop")
                    .icon(gpui_kit::assets::IconName::SquareStop)
                    .label("Stop")
                    .danger()
                    .compact()
                    .disabled(!self.running)
                    .tooltip("Stop running script")
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(StopRequested))),
            )
            .child(
                Button::new("save")
                    .icon(IconName::Check)
                    .label("Save")
                    .ghost()
                    .compact()
                    .disabled(!can_save)
                    .tooltip("Save (⌘S)")
                    .on_click(cx.listener(|this, _, window, cx| this.save(window, cx))),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(if self.current_script.is_some() {
                        fg
                    } else {
                        muted_fg
                    })
                    .child(title),
            )
            .child(if dirty {
                div().size_2().rounded_full().bg(warning).into_any_element()
            } else {
                div().into_any_element()
            });

        // 没载入脚本就没东西可编辑：显示提示 + 快捷键速查表，而不是空编辑器。
        let editor_area: AnyElement = if can_run {
            Editor::new(&self.editor)
                .appearance(false)
                .bordered(false)
                .size_full()
                .into_any_element()
        } else {
            let shortcuts = [
                ("Cmd + /", "Toggle line comment"),
                ("Cmd + Option + /", "Toggle block comment"),
                ("Cmd + Shift + K", "Delete line"),
                ("Shift + Option + ↑ / ↓", "Duplicate line"),
                ("Option + ↑ / ↓", "Move line"),
                ("Cmd + Enter", "Run script"),
                ("Cmd + S", "Save script"),
            ];
            div()
                .size_full()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap_3()
                .child(
                    div()
                        .text_3xl()
                        .text_color(muted_fg)
                        .child(gpui_kit::assets::IconName::FileCode),
                )
                .child(div().text_lg().text_color(fg).child("No script selected"))
                .child(
                    div()
                        .text_sm()
                        .text_color(muted_fg)
                        .child("Select a script from the list to start editing."),
                )
                .child(
                    div()
                        .mt_4()
                        .p_4()
                        .rounded_md()
                        .border_1()
                        .border_color(border)
                        .flex()
                        .flex_col()
                        .gap_2()
                        .child(div().text_sm().text_color(muted_fg).child("Shortcuts"))
                        .children(shortcuts.iter().map(|(keys, desc)| {
                            div()
                                .flex()
                                .items_center()
                                .gap_3()
                                .child(div().w(px(168.)).text_sm().text_color(fg).child(*keys))
                                .child(div().text_sm().text_color(muted_fg).child(*desc))
                        })),
                )
                .into_any_element()
        };

        div()
            .size_full()
            .flex()
            .flex_col()
            // action 从聚焦的编辑器向上冒泡到这个祖先容器处理。
            .on_action(cx.listener(Self::on_toggle_comment))
            .on_action(cx.listener(Self::on_toggle_block_comment))
            .on_action(cx.listener(Self::on_delete_line))
            .on_action(cx.listener(Self::on_duplicate_line_up))
            .on_action(cx.listener(Self::on_duplicate_line_down))
            .on_action(cx.listener(Self::on_move_line_up))
            .on_action(cx.listener(Self::on_move_line_down))
            .child(toolbar)
            .child(div().flex_1().min_h_0().child(editor_area))
    }
}
