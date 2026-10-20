//! Document lifecycle for the editor panel: loading a script from disk, saving
//! it back, running it, and mirroring server-pushed LSP diagnostics into the
//! editor.

use std::time::Duration;

use gpui_kit::component::WindowExt;
use gpui_kit::*;

use super::{EditorPanel, RunRequested};
use crate::lsp::path_to_uri;
use crate::lsp::providers::DocHandle;
use crate::model::script::Script;
use crate::model::script_store;

impl EditorPanel {
    /// Push the current editor content to the language server (full sync).
    pub(super) fn sync_did_change(&mut self, cx: &mut Context<Self>) {
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
    pub(super) async fn diagnostics_loop(this: WeakEntity<Self>, cx: &mut AsyncApp) {
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
    pub(super) fn load_script(
        &mut self,
        script: &Script,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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
        let uri = path_to_uri(&script.path);
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
        // Reflect the saved content back into the cached Script.
        if let Some(s) = self.current_script.as_mut() {
            s.content = content;
        }
        self.dirty = false;
    }

    pub(super) fn has_runnable(&self) -> bool {
        self.current_script.is_some()
    }
}
