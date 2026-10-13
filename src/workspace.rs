use std::path::PathBuf;
use std::sync::Arc;

use gpui_kit::component::dock::{DockArea, DockLayout, DockSkin, panel_handle};
use gpui_kit::*;

use crate::console_channel::ConsoleChannel;
use crate::host_module::HostRegistry;
use crate::panels::editor_panel::RunRequested;
use crate::panels::script_list_panel::ScriptSelected;
use crate::panels::{ConsolePanel, EditorPanel, ScriptListPanel};

gpui_kit::actions!(scripter, [SaveScript, RunScript, NewScript]);

pub struct Workspace {
    dock_area: Entity<DockArea>,
    #[allow(dead_code)]
    scripts_dir: PathBuf,
    #[allow(dead_code)]
    script_list: Entity<ScriptListPanel>,
    editor: Entity<EditorPanel>,
    console: Entity<ConsolePanel>,
    /// Cloneable handle kept so the runner can push output to the Console
    /// panel from a worker thread.
    console_channel: ConsoleChannel,
    #[allow(dead_code)]
    registry: Arc<HostRegistry>,
    _sub_select: Subscription,
    _sub_run: Subscription,
}

impl Workspace {
    pub fn new(
        window: &mut Window,
        cx: &mut Context<Self>,
        registry: Arc<HostRegistry>,
        lsp: Result<Arc<crate::lsp::LspClient>, String>,
    ) -> Self {
        let scripts_dir = crate::script_store::scripts_dir();

        // Window-level shortcuts: Cmd+S save, Cmd+Enter run, Cmd+N new.
        cx.bind_keys([
            KeyBinding::new("cmd-s", SaveScript, None),
            KeyBinding::new("cmd-enter", RunScript, None),
            KeyBinding::new("cmd-n", NewScript, None),
        ]);

        let dock_area = cx
            .new(|cx| DockArea::new("main", Some(1), window, cx).with_renderer(DockSkin::new(cx)));

        let script_list = cx.new(|cx| ScriptListPanel::new(window, cx));
        let (console_channel, console_rx) = ConsoleChannel::new();
        match &lsp {
            Ok(_) => {
                console_channel.info("LSP: typescript-language-server connected");
            }
            Err(reason) => {
                console_channel.warn(&format!(
                    "LSP unavailable: {reason} — install with: npm i -g typescript-language-server typescript"
                ));
            }
        }
        let console = cx.new(|cx| ConsolePanel::new(window, console_rx, cx));
        let editor = cx.new(|cx| EditorPanel::new(window, cx, lsp.ok()));

        // Script selection -> load into the editor (deferred to render, which
        // owns the window that EditorState::set_value requires).
        let sub_select = cx.subscribe(
            &script_list,
            move |this, _list, event: &ScriptSelected, cx| {
                this.editor
                    .update(cx, |ed, cx| ed.queue_load(event.0.clone(), cx));
            },
        );

        // Run request -> execute the script via rquickjs on a worker thread.
        let sub_run = cx.subscribe(&editor, move |this, _ed, event: &RunRequested, _cx| {
            let content = event.content.clone();
            let language = event.language;
            let file_name = event.file_name.clone();
            let console = this.console_channel.clone();
            let scripts_dir = this.scripts_dir.clone();
            let registry = this.registry.clone();
            std::thread::spawn(move || {
                crate::runner::run_script(
                    &content,
                    language,
                    &file_name,
                    scripts_dir,
                    &registry,
                    console,
                );
            });
        });

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
            scripts_dir,
            script_list,
            editor,
            console,
            console_channel,
            registry,
            _sub_select: sub_select,
            _sub_run: sub_run,
        }
    }
}

impl Workspace {
    fn on_save(&mut self, _: &SaveScript, window: &mut Window, cx: &mut Context<Self>) {
        self.editor.update(cx, |ed, cx| ed.save(window, cx));
    }

    fn on_run(&mut self, _: &RunScript, window: &mut Window, cx: &mut Context<Self>) {
        self.editor.update(cx, |ed, cx| ed.run_current(window, cx));
    }

    fn on_new(&mut self, _: &NewScript, _window: &mut Window, cx: &mut Context<Self>) {
        self.script_list.update(cx, |list, cx| list.create_new(cx));
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Keep the window title in sync with the loaded script.
        let title = match self.editor.read(cx).current_script() {
            Some(script) => format!("Scripter — {}", script.file_name),
            None => "Scripter".to_string(),
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
