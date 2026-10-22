//! Application-level script run service.
//!
//! Owns the full run chain — worker thread, cancel token, interaction bridge
//! and the standby/progress/confirm window — so both the launcher and the
//! script manager can start runs without depending on each other's windows.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender};
use futures::channel::oneshot;
use gpui_kit::*;

use super::interaction::{ConfirmChoice, InteractionBridge, PickKind, ProgressSlot, UiRequest};
use super::runner::{CancelToken, RunEnv};
use crate::console_channel::ConsoleChannel;
use crate::host::HostRegistry;
use crate::model::script::Language;
use crate::ui::interaction_window::{InteractionEvent, InteractionState, InteractionWindow};

/// How often the interaction drain runs while a script is executing.
const INTERACTION_TICK: Duration = Duration::from_millis(50);

/// Progress that finishes faster than this never opens a window — otherwise
/// short operations flash one on screen for a single frame.
const PROGRESS_WINDOW_DELAY: Duration = Duration::from_millis(300);

/// Standby / progress window size.
const INTERACTION_WINDOW_SIZE: (f32, f32) = (420., 150.);

/// A `pickPath` call waiting on the native file picker.
type PickPending = (
    Sender<Option<PathBuf>>,
    oneshot::Receiver<Result<Option<Vec<PathBuf>>>>,
);

/// Bookkeeping for the single script run currently in flight.
struct RunState {
    cancel: CancelToken,
    /// Set by the worker thread once `run_script` returns.
    done: Arc<AtomicBool>,
}

/// Emitted when a run starts (`true`) or finishes (`false`), so UI like the
/// editor toolbar can follow the service without owning it.
pub struct RunStateChanged(pub bool);

pub struct RunService {
    scripts_dir: PathBuf,
    registry: Arc<HostRegistry>,
    /// Cloneable handle so the runner can push output to the Console panel
    /// from a worker thread.
    console_channel: ConsoleChannel,
    /// `None` while idle; only one run at a time.
    running: Option<RunState>,
    /// Bridge of the in-flight run. Kept on the UI side so the drain loop can
    /// read progress and mark focus as confirmed.
    interaction_bridge: InteractionBridge,
    /// Script → UI requests of the in-flight run.
    interaction_rx: Receiver<UiRequest>,
    /// Progress slot shared with the in-flight run.
    progress: ProgressSlot,
    /// The standby / progress / confirm window, when one is up.
    interaction_window: Option<(AnyWindowHandle, Entity<InteractionWindow>)>,
    /// `confirm` waiting for an answer.
    pending_confirm: Option<(String, Sender<ConfirmChoice>)>,
    /// `requestFocus` waiting for the user to pick a window.
    pending_focus: Option<(String, Sender<()>)>,
    /// `pickPath` waiting on the native picker.
    pending_pick: Option<PickPending>,
    /// Whether the standby window has been seen active at least once; only then
    /// does losing activation mean "the user picked another window".
    focus_activated: bool,
    /// When progress first appeared, for the no-flash delay.
    progress_since: Option<Instant>,
    _sub_interaction: Option<Subscription>,
    /// Polls `running.done` so listeners leave the "running" state.
    _run_task: Option<Task<()>>,
    /// Drains `interaction_rx` and keeps the interaction window in sync.
    _interaction_task: Option<Task<()>>,
}

impl EventEmitter<RunStateChanged> for RunService {}

impl RunService {
    pub fn new(
        scripts_dir: PathBuf,
        registry: Arc<HostRegistry>,
        console_channel: ConsoleChannel,
    ) -> Self {
        // Idle placeholders, replaced by the first run's own bridge.
        let (interaction_bridge, interaction_rx) = InteractionBridge::new(CancelToken::new());
        let progress = interaction_bridge.progress_slot();
        Self {
            scripts_dir,
            registry,
            console_channel,
            running: None,
            interaction_bridge,
            interaction_rx,
            progress,
            interaction_window: None,
            pending_confirm: None,
            pending_focus: None,
            pending_pick: None,
            focus_activated: false,
            progress_since: None,
            _sub_interaction: None,
            _run_task: None,
            _interaction_task: None,
        }
    }

    pub fn is_running(&self) -> bool {
        self.running.is_some()
    }

    /// Execute a script via rquickjs on a worker thread. One run at a time: a
    /// second request while busy is refused with a Console warning.
    pub fn start(
        &mut self,
        content: String,
        language: Language,
        file_name: String,
        cx: &mut Context<Self>,
    ) {
        if self.running.is_some() {
            self.console_channel
                .warn(format!("已有脚本在运行中，忽略本次执行：{file_name}"));
            return;
        }
        let cancel = CancelToken::new();
        let done = Arc::new(AtomicBool::new(false));
        self.running = Some(RunState {
            cancel: cancel.clone(),
            done: done.clone(),
        });
        cx.emit(RunStateChanged(true));

        // Fresh interaction bridge per run: the script side talks to the UI
        // through it (progress, confirm, pickPath, requestFocus).
        let (interaction, interaction_rx) = InteractionBridge::new(cancel.clone());
        self.progress = interaction.progress_slot();
        self.interaction_rx = interaction_rx;
        self.pending_confirm = None;
        self.pending_focus = None;
        self.pending_pick = None;
        self.focus_activated = false;
        self.progress_since = None;

        let console = self.console_channel.clone();
        let scripts_dir = self.scripts_dir.clone();
        let registry = self.registry.clone();
        self.interaction_bridge = interaction.clone();
        let env = RunEnv {
            scripts_dir,
            registry,
            console,
            cancel,
            interaction,
        };
        std::thread::spawn(move || {
            super::runner::run_script(&content, language, &file_name, env);
            done.store(true, Ordering::SeqCst);
        });

        // The worker thread cannot reach gpui entities, so poll for
        // completion (same pattern as EditorPanel::diagnostics_loop).
        self._run_task = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(150))
                    .await;
                let finished = this.update(cx, |svc, cx| svc.poll_run(cx)).unwrap_or(true);
                if finished {
                    break;
                }
            }
        }));

        // Answer interaction requests and keep the interaction window in
        // sync. Runs faster than the run poll above: it drives the standby
        // release check and the progress bar.
        self._interaction_task = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(INTERACTION_TICK).await;
                let finished = this
                    .update(cx, |svc, cx| svc.poll_interaction(cx))
                    .unwrap_or(true);
                if finished {
                    break;
                }
            }
        }));
    }

    /// Flip the token; the runner aborts at the next interrupt check or when
    /// the racing wait wins.
    pub fn stop(&mut self) {
        if let Some(run) = &self.running {
            run.cancel.cancel();
        }
    }

    /// Returns true once the in-flight run has finished, after notifying
    /// listeners and tearing down interaction state.
    fn poll_run(&mut self, cx: &mut Context<Self>) -> bool {
        let finished = self
            .running
            .as_ref()
            .map(|run| run.done.load(Ordering::SeqCst))
            .unwrap_or(true);
        if finished {
            self.running = None;
            cx.emit(RunStateChanged(false));
            // The script side is gone: drop every pending request and close the
            // interaction window, so nothing is left waiting on a dead run.
            self.pending_confirm = None;
            self.pending_focus = None;
            self.pending_pick = None;
            self.progress_since = None;
            *self.progress.lock() = None;
            self.close_interaction_window(cx);
        }
        finished
    }

    /// Drain interaction requests, resolve what the UI can resolve and keep the
    /// interaction window in sync. Returns true once the run has ended.
    fn poll_interaction(&mut self, cx: &mut Context<Self>) -> bool {
        let finished = self.running.is_none();

        while let Ok(request) = self.interaction_rx.try_recv() {
            match request {
                UiRequest::Confirm { message, reply } => {
                    self.pending_confirm = Some((message, reply));
                }
                UiRequest::PickPath {
                    prompt,
                    kind,
                    reply,
                } => {
                    // The native picker is modal and cannot be driven from here,
                    // so start it and remember the reply for later ticks.
                    let receiver = cx.prompt_for_paths(PathPromptOptions {
                        files: kind == PickKind::File,
                        directories: kind == PickKind::Dir,
                        multiple: false,
                        prompt: Some(prompt.into()),
                    });
                    self.pending_pick = Some((reply, receiver));
                }
                UiRequest::RequestFocus { hint, reply } => {
                    self.pending_focus = Some((hint, reply));
                    self.focus_activated = false;
                }
            }
        }

        self.poll_pick();
        self.poll_focus(cx);
        self.sync_interaction_window(cx);

        finished
    }

    /// Forward the native picker's result once the user closes it.
    fn poll_pick(&mut self) {
        let Some((_, receiver)) = &mut self.pending_pick else {
            return;
        };
        let picked = match receiver.try_recv() {
            // Still on screen: keep waiting, the next tick checks again.
            Ok(None) => return,
            Ok(Some(Ok(paths))) => paths.and_then(|paths| paths.into_iter().next()),
            Ok(Some(Err(err))) => {
                self.console_channel
                    .warn(format!("Pick path failed: {err}"));
                None
            }
            // The sender was dropped: treat it as a cancel, not a failure.
            Err(oneshot::Canceled) => None,
        };
        if let Some((reply, _)) = self.pending_pick.take() {
            let _ = reply.send(picked);
        }
    }

    /// Release a focus standby once the user moves activation to another window.
    fn poll_focus(&mut self, cx: &mut Context<Self>) {
        if self.pending_focus.is_none() {
            return;
        }
        // A window that is not up yet cannot be inactive, which is why the
        // release only arms after it has been seen active at least once.
        let active = self
            .interaction_window
            .as_ref()
            .and_then(|(handle, _)| {
                handle
                    .update(cx, |_, window, _| window.is_window_active())
                    .ok()
            })
            .unwrap_or(false);
        if active {
            self.focus_activated = true;
            return;
        }
        if !self.focus_activated {
            return;
        }
        if let Some((_, reply)) = self.pending_focus.take() {
            let _ = reply.send(());
        }
        self.interaction_bridge.confirm_focus();
    }

    /// Show, update or close the interaction window based on what is pending.
    fn sync_interaction_window(&mut self, cx: &mut Context<Self>) {
        let state = if let Some((message, _)) = &self.pending_confirm {
            InteractionState::Confirm {
                message: message.clone(),
            }
        } else if let Some((hint, _)) = &self.pending_focus {
            InteractionState::Focus { hint: hint.clone() }
        } else if let Some(snapshot) = self.progress.lock().clone() {
            // Progress shorter than the delay is not worth a window.
            let since = *self.progress_since.get_or_insert_with(Instant::now);
            if self.interaction_window.is_none() && since.elapsed() < PROGRESS_WINDOW_DELAY {
                return;
            }
            InteractionState::Progress { snapshot }
        } else {
            self.progress_since = None;
            InteractionState::Idle
        };

        if state == InteractionState::Idle {
            self.close_interaction_window(cx);
            return;
        }

        // Whether this state should take focus on creation. Focus/Confirm need
        // focus so the standby can detect a loss-of-activation and Confirm
        // catches keystrokes; Progress must NOT steal focus or the simulated
        // keystrokes will land on our own window.
        let wants_focus = !matches!(state, InteractionState::Progress { .. });

        let need_reopen = match &self.interaction_window {
            None => true,
            Some((_, view)) => {
                let current_state = view.read(cx).state();
                // Same kind of state — just update the content, window keeps
                // its existing focus (which is correct for a same-type transition).
                std::mem::discriminant(current_state) != std::mem::discriminant(&state)
                    || (wants_focus != current_state.wants_focus())
            }
        };
        if need_reopen {
            self.close_interaction_window(cx);
            self.open_interaction_window(cx, wants_focus);
        }
        if let Some((_, window)) = &self.interaction_window {
            window.update(cx, |view, cx| view.set_state(state, cx));
        }
    }

    /// Open the always-on-top interaction window. `take_focus` should be `false`
    /// for the Progress state so simulated keystrokes don't land on the overlay
    /// itself; Focus/Confirm need it to be `true`.
    fn open_interaction_window(&mut self, cx: &mut Context<Self>, take_focus: bool) {
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::centered(
                size(px(INTERACTION_WINDOW_SIZE.0), px(INTERACTION_WINDOW_SIZE.1)),
                cx,
            )),
            titlebar: Some(TitlebarOptions {
                title: Some("Bolt".into()),
                appears_transparent: false,
                traffic_light_position: None,
            }),
            focus: take_focus,
            kind: WindowKind::PopUp,
            is_resizable: false,
            ..Default::default()
        };
        let opened =
            gpui_kit::open_window(options, cx, |_window, cx| cx.new(InteractionWindow::new));
        match opened {
            Ok((handle, view)) => {
                self._sub_interaction = Some(cx.subscribe(
                    &view,
                    |this, _view, event: &InteractionEvent, cx| {
                        this.on_interaction_event(*event, cx);
                    },
                ));
                self.interaction_window = Some((handle, view));
            }
            Err(e) => {
                self.console_channel.warn(format!("无法打开交互窗口：{e}"));
            }
        }
    }

    fn close_interaction_window(&mut self, cx: &mut Context<Self>) {
        let Some((handle, _)) = self.interaction_window.take() else {
            return;
        };
        self.focus_activated = false;
        let _ = handle.update(cx, |_, window, _| window.remove_window());
    }

    /// Buttons in the interaction window are answered here, so the reply
    /// channels only ever have one owner.
    fn on_interaction_event(&mut self, event: InteractionEvent, _cx: &mut Context<Self>) {
        match event {
            InteractionEvent::ConfirmYes | InteractionEvent::ConfirmNo => {
                let choice = if event == InteractionEvent::ConfirmYes {
                    ConfirmChoice::Yes
                } else {
                    ConfirmChoice::No
                };
                if let Some((_, reply)) = self.pending_confirm.take() {
                    let _ = reply.send(choice);
                }
            }
            InteractionEvent::Abort => {
                // Hand the script its abort reason, then stop the run; the token
                // also breaks any await the request is still sitting in.
                if let Some((_, reply)) = self.pending_confirm.take() {
                    let _ = reply.send(ConfirmChoice::Abort);
                }
                self.pending_focus = None;
                if let Some(run) = &self.running {
                    run.cancel.cancel();
                }
            }
        }
    }
}
