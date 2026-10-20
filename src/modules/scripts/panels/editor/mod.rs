//! The editor panel: hosts the code editor, its LSP providers, and the script
//! currently loaded into it.
//!
//! Document plumbing (load / save / run / diagnostics) lives in `document`,
//! the editing shortcuts in `actions`. All fields stay here and are private:
//! child modules can read and write them, which keeps the split free of extra
//! accessor indirection.

use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use gpui_kit::base::dock::PanelEvent;
use gpui_kit::component::ActiveTheme;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::dock::{BasePanel, Panel};
use gpui_kit::component::input::{Editor, EditorState, InputEvent, TabSize};
use gpui_kit::component::{Disableable, IconName};
use gpui_kit::*;
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

/// Emitted when the user clicks Run.
#[derive(Clone, Debug)]
pub struct RunRequested {
    pub content: String,
    pub language: Language,
    pub file_name: String,
}

/// Emitted when the user clicks Stop, asking the host to abort the running
/// script.
#[derive(Clone, Debug)]
pub struct StopRequested;

pub struct EditorPanel {
    focus_handle: FocusHandle,
    scripts_dir: PathBuf,
    editor: Entity<EditorState>,
    current_script: Option<Script>,
    dirty: bool,
    /// True while a script started from this panel is executing; drives the
    /// Run/Stop enabled state. Owned by the host (Workspace), which calls
    /// `set_running`.
    running: bool,
    pending_load: Option<Script>,
    lsp: Option<Arc<LspClient>>,
    doc: Arc<Mutex<Option<DocHandle>>>,
    _diagnostics_task: Option<Task<()>>,
    _subscription: Subscription,
}

impl EditorPanel {
    pub fn new(window: &mut Window, cx: &mut Context<Self>, lsp: Option<Arc<LspClient>>) -> Self {
        // Editing shortcuts. `Some("Input")` matches the editor's key context
        // ("Input mode=editor"), so these only fire while the editor is focused
        // and never shadow unrelated bindings elsewhere in the app. Note that
        // `cmd-shift-/` is unreachable on macOS (the shifted punctuation key
        // reports as `cmd-?`), hence the `cmd-alt-/` block-comment binding.
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

        // Register the LSP providers on the editor when the language server
        // is available.
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
                // Dirty means the editor content differs from the loaded
                // script's saved content. Comparing content (instead of
                // assuming every Change is a user edit) keeps `set_value`
                // during script loading from marking the editor dirty.
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
                // Stop uses the full Lucide catalog (assets::IconName); the
                // component enum only carries a fixed compatibility subset and
                // has no SquareStop variant.
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

        // With no script loaded there is nothing to edit, so show a hint plus a
        // shortcut cheat-sheet instead of an empty editor.
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
            // Actions bubble from the focused editor up to this ancestor.
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
