//! Raycast-style launcher: a floating command palette over built-in commands,
//! scripts and external extensions, toggled by a global hotkey.

use std::time::Duration;

use crossbeam_channel::Sender;
use gpui_kit::component::ActiveTheme;
use gpui_kit::component::IndexPath;
use gpui_kit::component::command::{Command, CommandGroup, CommandItem, CommandState};
use gpui_kit::*;

use crate::console_channel::ConsoleChannel;
use crate::model::command_model::{CommandAction, CommandGroups, collect_commands};

/// Poll interval for the auto-close-on-blur check.
const BLUR_TICK: Duration = Duration::from_millis(100);

/// What the app shell (main) should do after a command is confirmed.
///
/// The request travels through a channel rather than an entity event: the
/// confirmed command closes the launcher window in the same callback, and an
/// entity event is dispatched only after that — by then the emitting view is
/// gone, so its subscriber is dropped along with it.
pub enum LauncherRequest {
    /// Open or focus the script manager; `new_script` starts its creation flow.
    OpenManager { new_script: bool },
    /// Run a script or extension entry file.
    RunFile(std::path::PathBuf),
    /// Open (or focus) a UI extension's own gpui-shell window.
    OpenExtensionUi {
        ext: crate::model::extension::UiExtension,
    },
}

pub struct Launcher {
    state: Entity<CommandState>,
    commands: CommandGroups,
    /// Drained by the app shell, which owns window and run lifecycle.
    requests: Sender<LauncherRequest>,
    /// Auto-close-on-blur poll; ends when the view is dropped.
    _blur_task: Task<()>,
}

impl Launcher {
    pub fn new(
        window: &mut Window,
        cx: &mut Context<Self>,
        requests: Sender<LauncherRequest>,
        console: ConsoleChannel,
    ) -> Self {
        let state = cx.new(|cx| CommandState::new(window, cx));
        // Start with the caret in the search field.
        let focus = state.read(cx).focus_handle(cx);
        focus.focus(window, cx);

        // Re-collect on every open: scripts and extensions may have changed
        // since the last time the launcher was up.
        let (commands, errors) = collect_commands(
            &crate::model::script_store::scripts_dir(),
            &crate::model::extension::extensions_dir(),
        );
        for error in &errors {
            console.warn(format!(
                "扩展加载失败 {}：{}",
                error.dir.display(),
                error.message
            ));
        }

        let window_handle = window.window_handle();

        // Raycast behavior: once the window has been active, losing activation
        // closes it. Arming only after the first activation avoids closing
        // during the initial focus hand-off.
        let blur_task = cx.spawn(async move |this, cx| {
            let mut was_active = false;
            loop {
                cx.background_executor().timer(BLUR_TICK).await;
                let blurred = this
                    .update(cx, |_, cx| {
                        let active = window_handle
                            .update(cx, |_, window, _| window.is_window_active())
                            .unwrap_or(false);
                        if active {
                            was_active = true;
                        }
                        was_active && !active
                    })
                    .unwrap_or(true);
                if blurred {
                    let _ = this.update(cx, |_, cx| {
                        let _ = window_handle.update(cx, |_, window, _| window.remove_window());
                    });
                    break;
                }
            }
        });

        Self {
            state,
            commands,
            requests,
            _blur_task: blur_task,
        }
    }

    fn on_confirm(&mut self, index: IndexPath, window: &mut Window) {
        let Some(command) = self.commands.resolve(index.section, index.row) else {
            return;
        };
        let request = match &command.action {
            CommandAction::OpenManager => LauncherRequest::OpenManager { new_script: false },
            CommandAction::NewScript => LauncherRequest::OpenManager { new_script: true },
            CommandAction::RunScript(path) | CommandAction::RunExtension { entry: path } => {
                LauncherRequest::RunFile(path.clone())
            }
            CommandAction::OpenExtensionUi { ext } => {
                LauncherRequest::OpenExtensionUi { ext: ext.clone() }
            }
        };
        // If the receiver is gone the app is shutting down: nothing left to do.
        let _ = self.requests.send(request);
        window.remove_window();
    }
}

impl Render for Launcher {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // A weak handle: the closure is stored in CommandState (a child of
        // this view), so a strong one would keep both entities alive forever.
        let this = cx.entity().downgrade();
        let mut command = Command::new(&self.state)
            // Fill the window so the list (flex_1) stretches instead of
            // leaving a blank strip below the footer; raise the list's cap
            // accordingly (default 18.75rem is shorter than this window).
            .h_full()
            .max_h(rems(40.))
            .placeholder("Search scripts or commands...")
            .on_confirm(move |index, window, app| {
                let _ = this.update(app, |this, _cx| this.on_confirm(index, window));
            })
            .on_cancel(|window, _cx| {
                window.remove_window();
            })
            .footer(|state, _window, cx| {
                div()
                    .px_3()
                    .py_2()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(format!("{} 个命令", state.matched_count()))
                    .into_any_element()
            });

        for (heading, commands) in &self.commands.groups {
            let mut group = CommandGroup::new().label(heading.clone());
            for cmd in commands {
                // The id is searchable too, so `ext.demo.go` finds the command.
                let mut keywords = vec![cmd.id.clone()];
                if let Some(subtitle) = &cmd.subtitle {
                    keywords.push(subtitle.clone());
                }
                let item = CommandItem::new()
                    .label(cmd.title.clone())
                    .icon(cmd.icon)
                    .keywords(keywords);
                group = group.item(item);
            }
            command = command.group(group);
        }

        div().size_full().child(command)
    }
}
