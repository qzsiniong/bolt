//! 交互窗口的生命周期管理：待命/进度/确认浮窗与 `pickPath` 原生选择器。
//!
//! 从 [`super::run_service::RunService`] 拆出来的纯 UI 侧状态机：每个运行
//! 回合由 `begin_run` 注入新的 bridge / 请求流 / 取消令牌，`drain` 每个
//! tick 被调一次，负责消化脚本侧请求、开关窗口、回填 reply 通道。它不关心
//! 「现在跑的是什么」——脚本与插件命令共用同一套。

use std::path::PathBuf;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender};
use futures::channel::oneshot;
use gpui_kit::*;

use super::interaction::{ConfirmChoice, InteractionBridge, PickKind, ProgressSlot, UiRequest};
use super::runner::CancelToken;
use crate::console_channel::ConsoleChannel;
use crate::ui::interaction_window::{InteractionEvent, InteractionState, InteractionWindow};

/// How often the interaction drain runs while a script is executing.
pub(crate) const INTERACTION_TICK: Duration = Duration::from_millis(50);

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

/// 交互浮窗管理器：一轮运行一个实例状态（`begin_run` 重置）。
pub(crate) struct InteractionWindowManager {
    /// Bridge of the in-flight run. Kept on the UI side so the drain loop can
    /// read progress and mark focus as confirmed.
    bridge: InteractionBridge,
    /// Script → UI requests of the in-flight run.
    rx: Receiver<UiRequest>,
    /// Progress slot shared with the in-flight run.
    progress: ProgressSlot,
    /// 当前运行的取消令牌：确认窗的「中止」按钮经它打断脚本侧的等待。
    cancel: CancelToken,
    console: ConsoleChannel,
    /// The standby / progress / confirm window, when one is up.
    window: Option<(AnyWindowHandle, Entity<InteractionWindow>)>,
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
}

impl InteractionWindowManager {
    /// Idle placeholders, replaced by the first run's own bridge.
    pub(crate) fn new(console: ConsoleChannel) -> Self {
        let (bridge, rx) = InteractionBridge::new(CancelToken::new());
        let progress = bridge.progress_slot();
        Self {
            bridge,
            rx,
            progress,
            cancel: CancelToken::new(),
            console,
            window: None,
            pending_confirm: None,
            pending_focus: None,
            pending_pick: None,
            focus_activated: false,
            progress_since: None,
        }
    }

    /// 当前交互窗口（`None` = 没开）。调用方用它补按钮事件订阅。
    pub(crate) fn window(&self) -> Option<&Entity<InteractionWindow>> {
        self.window.as_ref().map(|(_, view)| view)
    }

    /// 开始一轮运行：换上本轮的 bridge / 请求流 / 取消令牌，清掉上一轮的
    /// 残留状态。
    pub(crate) fn begin_run(
        &mut self,
        bridge: InteractionBridge,
        rx: Receiver<UiRequest>,
        cancel: CancelToken,
    ) {
        self.bridge = bridge;
        self.rx = rx;
        self.progress = self.bridge.progress_slot();
        self.cancel = cancel;
        self.pending_confirm = None;
        self.pending_focus = None;
        self.pending_pick = None;
        self.focus_activated = false;
        self.progress_since = None;
    }

    /// 一轮运行结束：丢弃所有待答复请求并关窗，不留任何等在死运行上的东西。
    pub(crate) fn end_run(&mut self, cx: &mut App) {
        self.pending_confirm = None;
        self.pending_focus = None;
        self.pending_pick = None;
        self.progress_since = None;
        *self.progress.lock() = None;
        self.close_window(cx);
    }

    /// Drain interaction requests, resolve what the UI can resolve and keep
    /// the interaction window in sync. Called once per tick while a run is up.
    pub(crate) fn drain(&mut self, cx: &mut App) {
        while let Ok(request) = self.rx.try_recv() {
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
        self.sync_window(cx);
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
                self.console.warn(format!("Pick path failed: {err}"));
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
    fn poll_focus(&mut self, cx: &mut App) {
        if self.pending_focus.is_none() {
            return;
        }
        // A window that is not up yet cannot be inactive, which is why the
        // release only arms after it has been seen active at least once.
        let active = self
            .window
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
        self.bridge.confirm_focus();
    }

    /// Show, update or close the interaction window based on what is pending.
    fn sync_window(&mut self, cx: &mut App) {
        let state = if let Some((message, _)) = &self.pending_confirm {
            InteractionState::Confirm {
                message: message.clone(),
            }
        } else if let Some((hint, _)) = &self.pending_focus {
            InteractionState::Focus { hint: hint.clone() }
        } else if let Some(snapshot) = self.progress.lock().clone() {
            // Progress shorter than the delay is not worth a window.
            let since = *self.progress_since.get_or_insert_with(Instant::now);
            if self.window.is_none() && since.elapsed() < PROGRESS_WINDOW_DELAY {
                return;
            }
            InteractionState::Progress { snapshot }
        } else {
            self.progress_since = None;
            InteractionState::Idle
        };

        if state == InteractionState::Idle {
            self.close_window(cx);
            return;
        }

        // Whether this state should take focus on creation. Focus/Confirm need
        // focus so the standby can detect a loss-of-activation and Confirm
        // catches keystrokes; Progress must NOT steal focus or the simulated
        // keystrokes will land on our own window.
        let wants_focus = !matches!(state, InteractionState::Progress { .. });

        let need_reopen = match &self.window {
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
            self.close_window(cx);
            self.open_window(cx, wants_focus);
        }
        if let Some((_, window)) = &self.window {
            window.update(cx, |view, cx| view.set_state(state, cx));
        }
    }

    /// Open the always-on-top interaction window. `take_focus` should be `false`
    /// for the Progress state so simulated keystrokes don't land on the overlay
    /// itself; Focus/Confirm need it to be `true`.
    fn open_window(&mut self, cx: &mut App, take_focus: bool) {
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
            Ok((handle, view)) => self.window = Some((handle, view)),
            Err(e) => self.console.warn(format!("无法打开交互窗口：{e}")),
        }
    }

    fn close_window(&mut self, cx: &mut App) {
        let Some((handle, _)) = self.window.take() else {
            return;
        };
        self.focus_activated = false;
        let _ = handle.update(cx, |_, window, _| window.remove_window());
    }

    /// Buttons in the interaction window are answered here, so the reply
    /// channels only ever have one owner. 由 [`RunService`] 的订阅转发进来。
    pub(crate) fn on_interaction_event(&mut self, event: InteractionEvent) {
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
                self.cancel.cancel();
            }
        }
    }
}
