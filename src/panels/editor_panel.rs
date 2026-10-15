use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use gpui_kit::base::dock::PanelEvent;
use gpui_kit::component::ActiveTheme;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::dock::{BasePanel, Panel};
use gpui_kit::component::input::{Editor, EditorState, InputEvent, Rope, TabSize};
use gpui_kit::component::{Disableable, IconName, WindowExt};
use gpui_kit::*;
use parking_lot::Mutex;

use crate::editor_ops;
use crate::lsp::LspClient;
use crate::lsp_providers::{DocHandle, LspBridge};
use crate::script::{Language, Script};
use crate::script_store;

// Editing shortcuts. The editor's key context is "Input mode=editor", so a
// binding under "Input" only fires while the editor has focus. Note that
// `Cmd+Shift+/` cannot be used for the block comment: on macOS the shifted
// punctuation key is reported as `cmd-?`, not `cmd-shift-/`.
gpui_kit::actions!(
    scripter,
    [
        ToggleComment,
        ToggleBlockComment,
        DeleteLine,
        DuplicateLineUp,
        DuplicateLineDown,
        MoveLineUp,
        MoveLineDown,
    ]
);

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

    /// Push the current editor content to the language server (full sync).
    fn sync_did_change(&mut self, cx: &mut Context<Self>) {
        let Some(client) = self.lsp.as_ref() else {
            return;
        };
        let text = self.editor.read(cx).value().to_string();
        let mut doc = self.doc.lock();
        let Some(handle) = doc.as_mut() else {
            return;
        };
        handle.version += 1;
        client.did_change(&handle.uri, handle.version, &text);
    }

    /// Poll server-pushed diagnostics and mirror them into the editor's
    /// diagnostic set (squiggly lines) for the currently open document.
    async fn diagnostics_loop(this: WeakEntity<Self>, cx: &mut AsyncApp) {
        loop {
            cx.background_executor()
                .timer(Duration::from_millis(250))
                .await;
            let alive = this
                .update(cx, |panel, cx| {
                    let Some(client) = panel.lsp.clone() else {
                        return;
                    };
                    let current_uri = panel.doc.lock().as_ref().map(|d| d.uri.clone());
                    let mut changed = false;
                    for published in client.take_diagnostics() {
                        if current_uri.as_deref() != Some(published.uri.as_str()) {
                            continue;
                        }
                        panel.editor.update(cx, |state, ecx| {
                            let rope = state.text().clone();
                            if let Some(set) = state.diagnostics_mut() {
                                // reset syncs the internal rope AND clears
                                // stale diagnostics before pushing fresh ones.
                                set.reset(&rope);
                                for diagnostic in published.diagnostics {
                                    set.push(diagnostic);
                                }
                                state.refresh(ecx);
                            }
                        });
                        changed = true;
                    }
                    if changed {
                        cx.notify();
                    }
                })
                .is_ok();
            if !alive {
                break;
            }
        }
    }

    pub fn editor(&self) -> &Entity<EditorState> {
        &self.editor
    }

    pub fn current_script(&self) -> Option<&Script> {
        self.current_script.as_ref()
    }

    /// Track whether a script started here is executing. Driven by the host
    /// (Workspace) when a run starts and completes, so the toolbar can switch
    /// between Run and Stop.
    pub fn set_running(&mut self, running: bool, cx: &mut Context<Self>) {
        if self.running != running {
            self.running = running;
            cx.notify();
        }
    }

    /// Whether the editor content differs from the script saved on disk.
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Queue a script to be loaded on the next render (render has access to
    /// the window, which `set_value` requires — subscribe callbacks do not).
    pub fn queue_load(&mut self, script: Script, cx: &mut Context<Self>) {
        self.pending_load = Some(script);
        cx.notify();
    }

    /// Load a script's content into the editor and switch the highlighter.
    ///
    /// Unsaved edits are intentionally *not* auto-saved here: switching away
    /// discards them, which is what the "unsaved changes will be lost"
    /// confirmation warns about.
    pub fn load_script(&mut self, script: &Script, window: &mut Window, cx: &mut Context<Self>) {
        // The list panel holds a stale snapshot, so always re-read from disk
        // to pick up content saved since the list was built.
        let script = match script_store::read_script(&self.scripts_dir, &script.file_name) {
            Ok(fresh) => fresh,
            Err(_) => script.clone(),
        };
        self.current_script = Some(script.clone());
        self.dirty = false;

        // Sync the LSP document: close the old one, open the new one.
        let language_id = script.language.as_str();
        let uri = crate::lsp::path_to_uri(&script.path);
        if let Some(client) = self.lsp.as_ref() {
            let old_uri = self.doc.lock().as_ref().map(|d| d.uri.clone());
            if let Some(old) = old_uri.filter(|old| *old != uri) {
                client.did_close(&old);
            }
            client.did_open(&uri, language_id, &script.content);
            *self.doc.lock() = Some(DocHandle { uri, version: 0 });
        }

        let lang = script.language;
        let content = script.content.clone();
        self.editor.update(cx, |state, cx| {
            state.set_value(content.as_str(), window, cx);
            state.set_highlighter(lang.as_str(), cx);
            // The set's internal rope must match the new text for
            // position -> offset mapping of incoming diagnostics.
            let rope = state.text().clone();
            if let Some(set) = state.diagnostics_mut() {
                set.reset(&rope);
            }
        });
        cx.notify();
    }

    /// Run the currently loaded script (toolbar button / Cmd+Enter).
    pub fn run_current(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.running || self.current_script.is_none() {
            return;
        }
        if self.dirty {
            // Unsaved edits: ask whether to save first. "No" runs the last
            // saved version instead of the text currently in the editor.
            let editor = cx.entity();
            window.open_alert_dialog(cx, move |alert, _, _| {
                alert
                    .title("未保存的修改")
                    .description(
                        "脚本有未保存的修改。先保存再运行吗？（否 = 直接运行已保存的版本）",
                    )
                    .show_cancel(true)
                    .ok_text("保存并运行")
                    .cancel_text("否，运行已保存版本")
                    .on_ok({
                        let editor = editor.clone();
                        move |_, window, cx| {
                            editor.update(cx, |ed, ecx| {
                                ed.save(window, ecx);
                                ed.emit_run_edited(ecx);
                            });
                            true
                        }
                    })
                    .on_cancel({
                        let editor = editor.clone();
                        move |_, _, cx| {
                            editor.update(cx, |ed, ecx| ed.emit_run_saved(ecx));
                            true
                        }
                    })
            });
            return;
        }
        self.emit_run_edited(cx);
    }

    /// Ask the host to run the text currently in the editor.
    fn emit_run_edited(&mut self, cx: &mut Context<Self>) {
        let Some(script) = self.current_script.clone() else {
            return;
        };
        let content = self.editor.read(cx).value().to_string();
        cx.emit(RunRequested {
            content,
            language: script.language,
            file_name: script.file_name,
        });
    }

    /// Ask the host to run the last version saved to disk, ignoring edits.
    fn emit_run_saved(&mut self, cx: &mut Context<Self>) {
        let Some(script) = self.current_script.clone() else {
            return;
        };
        cx.emit(RunRequested {
            content: script.content,
            language: script.language,
            file_name: script.file_name,
        });
    }

    /// Save the current script (toolbar button / Cmd+S).
    pub fn save(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        if !self.dirty || self.current_script.is_none() {
            return;
        }
        self.persist_current(cx);
        cx.notify();
    }

    /// Write the editor content back to the script's file on disk.
    fn persist_current(&mut self, cx: &mut Context<Self>) {
        if !self.dirty {
            return;
        }
        let Some(script) = self.current_script.clone() else {
            return;
        };
        let content = self.editor.read(cx).value().to_string();
        if let Err(e) = script_store::save_script(&self.scripts_dir, &script.file_name, &content) {
            eprintln!("save: {e}");
            return;
        }
        // Reflect the saved content back into the cached Script so future
        // `is_unsaved_content` checks stay accurate.
        if let Some(s) = self.current_script.as_mut() {
            s.content = content;
        }
        self.dirty = false;
    }

    fn has_runnable(&self) -> bool {
        self.current_script.is_some()
    }

    /// Apply a pure text transform to the editor: compute an edit from the
    /// current rope/selection, then replace that byte range and set the
    /// resulting selection. Changing the selection first turns
    /// [`EditorState::replace`] into a replace-arbitrary-range primitive.
    /// `replace` still emits `InputEvent::Change`, so dirty tracking and the
    /// LSP `did_change` sync keep working as usual.
    fn apply_op(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        compute: impl FnOnce(&Rope, std::ops::Range<usize>) -> Option<editor_ops::Edit>,
    ) {
        if self.current_script.is_none() {
            return;
        }
        self.editor.update(cx, |state, ecx| {
            let Some(edit) = compute(state.text(), state.selected_range()) else {
                return;
            };
            // Guard against no-op edits so they don't add a stray undo step.
            if edit.text == state.text().slice(edit.range.clone()).to_string() {
                return;
            }
            state.set_selected_range(edit.range.clone(), ecx);
            state.replace(edit.text.clone(), window, ecx);
            state.set_selected_range(edit.selection.clone(), ecx);
        });
    }

    fn on_toggle_comment(
        &mut self,
        _: &ToggleComment,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.apply_op(window, cx, editor_ops::toggle_line_comment);
    }

    fn on_toggle_block_comment(
        &mut self,
        _: &ToggleBlockComment,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.apply_op(window, cx, editor_ops::toggle_block_comment);
    }

    fn on_delete_line(&mut self, _: &DeleteLine, window: &mut Window, cx: &mut Context<Self>) {
        self.apply_op(window, cx, editor_ops::delete_lines);
    }

    fn on_duplicate_line_up(
        &mut self,
        _: &DuplicateLineUp,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.apply_op(window, cx, |rope, selection| {
            editor_ops::duplicate_lines(rope, selection, true)
        });
    }

    fn on_duplicate_line_down(
        &mut self,
        _: &DuplicateLineDown,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.apply_op(window, cx, |rope, selection| {
            editor_ops::duplicate_lines(rope, selection, false)
        });
    }

    fn on_move_line_up(&mut self, _: &MoveLineUp, window: &mut Window, cx: &mut Context<Self>) {
        self.apply_op(window, cx, |rope, selection| {
            editor_ops::move_lines(rope, selection, true)
        });
    }

    fn on_move_line_down(&mut self, _: &MoveLineDown, window: &mut Window, cx: &mut Context<Self>) {
        self.apply_op(window, cx, |rope, selection| {
            editor_ops::move_lines(rope, selection, false)
        });
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
