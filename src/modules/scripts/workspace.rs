use std::sync::Arc;

use crossbeam_channel::Receiver;
use gpui_kit::component::dock::{DockArea, DockLayout, DockSkin, panel_handle};
use gpui_kit::*;

use super::panels::editor::{RunRequested, StopRequested};
use super::panels::script_list_panel::ScriptSelected;
use super::panels::{ConsolePanel, EditorPanel, ScriptListPanel};
use crate::console_channel::ConsoleEntry;
use crate::runtime::run_service::{RunService, RunStateChanged};

gpui_kit::actions!(bolt, [SaveScript, RunScript, NewScript]);

/// The script manager window: dock with script list, editor and console.
/// Script execution lives in the app-level [`RunService`], so this window can
/// be closed without affecting runs started from the launcher.
pub struct Workspace {
    dock_area: Entity<DockArea>,
    script_list: Entity<ScriptListPanel>,
    editor: Entity<EditorPanel>,
    run_service: Entity<RunService>,
    _sub_select: Subscription,
    _sub_run: Subscription,
    _sub_stop: Subscription,
    _sub_run_state: Subscription,
}

impl Workspace {
    pub fn new(
        window: &mut Window,
        cx: &mut Context<Self>,
        lsp: Result<Arc<crate::lsp::LspClient>, String>,
        console_rx: Receiver<ConsoleEntry>,
        run_service: Entity<RunService>,
    ) -> Self {
        // Window-level shortcuts: Cmd+S save, Cmd+Enter run, Cmd+N new.
        cx.bind_keys([
            KeyBinding::new("cmd-s", SaveScript, None),
            KeyBinding::new("cmd-enter", RunScript, None),
            KeyBinding::new("cmd-n", NewScript, None),
        ]);

        let dock_area = cx
            .new(|cx| DockArea::new("main", Some(1), window, cx).with_renderer(DockSkin::new(cx)));

        let script_list = cx.new(|cx| ScriptListPanel::new(window, cx));
        let console = cx.new(|cx| ConsolePanel::new(window, console_rx, cx));
        let editor = cx.new(|cx| EditorPanel::new(window, cx, lsp.ok()));
        // Let the list panel warn about unsaved edits before it replaces the
        // script currently open in the editor.
        script_list.update(cx, |list, _| list.set_editor(editor.downgrade()));

        // Script selection -> load into the editor (deferred to render, which
        // owns the window that EditorState::set_value requires).
        let sub_select = cx.subscribe(
            &script_list,
            move |this, _list, event: &ScriptSelected, cx| {
                this.editor
                    .update(cx, |ed, cx| ed.queue_load(event.0.clone(), cx));
            },
        );

        // Run request -> hand the script to the app-level run service.
        let sub_run = cx.subscribe(&editor, move |this, _ed, event: &RunRequested, cx| {
            this.run_service.update(cx, |svc, cx| {
                svc.start(
                    event.content.clone(),
                    event.language,
                    event.file_name.clone(),
                    cx,
                );
            });
        });

        // Stop request -> flip the service's cancel token.
        let sub_stop = cx.subscribe(&editor, move |this, _ed, _: &StopRequested, cx| {
            this.run_service.update(cx, |svc, _| svc.stop());
        });

        // A run may already be in flight (started from the launcher): the
        // toolbar must reflect it immediately, not only on the next event.
        editor.update(cx, |ed, cx| {
            ed.set_running(run_service.read(cx).is_running(), cx)
        });

        // Follow the service's running state so the toolbar reflects runs
        // started from anywhere (editor button, Cmd+Enter or the launcher).
        let sub_run_state = cx.subscribe(
            &run_service,
            move |this, _svc, event: &RunStateChanged, cx| {
                this.editor.update(cx, |ed, cx| ed.set_running(event.0, cx));
            },
        );

        dock_area.update(cx, |area, dock_cx| {
            let layout = DockLayout::h_split()
                .child(
                    DockLayout::tabs().panel_view(panel_handle(script_list.clone()), dock_cx),
                    Some(px(240.)),
                )
                .child(
                    DockLayout::v_split()
                        .child(
                            DockLayout::tabs().panel_view(panel_handle(editor.clone()), dock_cx),
                            None,
                        )
                        .child(
                            DockLayout::tabs().panel_view(panel_handle(console.clone()), dock_cx),
                            Some(px(200.)),
                        ),
                    None,
                );

            area.set_center(layout, window, dock_cx);
        });

        Self {
            dock_area,
            script_list,
            editor,
            run_service,
            _sub_select: sub_select,
            _sub_run: sub_run,
            _sub_stop: sub_stop,
            _sub_run_state: sub_run_state,
        }
    }

    /// Run the new-script flow (used by the launcher's NewScript command).
    pub fn trigger_new_script(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.script_list
            .update(cx, |list, cx| list.create_new(window, cx));
    }

    fn on_save(&mut self, _: &SaveScript, window: &mut Window, cx: &mut Context<Self>) {
        self.editor.update(cx, |ed, cx| ed.save(window, cx));
    }

    fn on_run(&mut self, _: &RunScript, window: &mut Window, cx: &mut Context<Self>) {
        self.editor.update(cx, |ed, cx| ed.run_current(window, cx));
    }

    fn on_new(&mut self, _: &NewScript, window: &mut Window, cx: &mut Context<Self>) {
        self.script_list
            .update(cx, |list, cx| list.create_new(window, cx));
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Keep the window title in sync with the loaded script.
        let title = match self.editor.read(cx).current_script() {
            Some(script) => format!("Bolt — {}", script.file_name),
            None => "Bolt".to_string(),
        };
        window.set_window_title(&title);

        div()
            .size_full()
            .on_action(cx.listener(Self::on_save))
            .on_action(cx.listener(Self::on_run))
            .on_action(cx.listener(Self::on_new))
            .child(self.dock_area.clone())
    }
}
