//! 交互窗口的生命周期管理：待命/进度/确认浮窗与 `pickPath` 原生选择器。
//!
//! 从 [`super::run_service::RunService`] 拆出来的纯 UI 侧状态机：每个运行
//! 回合由 `begin_run` 注入新的 bridge / 请求流 / 取消令牌，`drain` 每个
//! tick 被调一次，负责消化脚本侧请求、开关窗口、回填 reply 通道。它不关心
//! 「现在跑的是什么」——脚本与扩展命令共用同一套。

use std::path::PathBuf;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender};
use futures::channel::oneshot;
use gpui_kit::{
    AnyWindowHandle, App, AppContext, Entity, PathPromptOptions, TitlebarOptions, WindowBounds,
    WindowKind, WindowOptions, px, size,
};

use super::interaction::{ConfirmChoice, InteractionBridge, PickKind, ProgressSlot, UiRequest};
use super::runner::CancelToken;
use crate::console_channel::ConsoleChannel;
use crate::ui::interaction_window::{InteractionEvent, InteractionState, InteractionWindow};

/// 脚本执行期间交互请求的排空间隔。
pub(crate) const INTERACTION_TICK: Duration = Duration::from_millis(50);

/// 比这更快的进度不开窗——否则短操作会在屏幕上闪一帧。
const PROGRESS_WINDOW_DELAY: Duration = Duration::from_millis(300);

/// 待命窗 / 进度窗的尺寸。
const INTERACTION_WINDOW_SIZE: (f32, f32) = (420., 150.);

/// 一次等待原生文件选择器返回的 `pickPath` 调用。
/// `Receiver` 是 gpui `prompt_for_paths` 的返回类型（futures-channel oneshot）。
type PickPending = (
    Sender<Option<PathBuf>>,
    oneshot::Receiver<anyhow::Result<Option<Vec<PathBuf>>>>,
);

/// 交互浮窗管理器：一轮运行一个实例状态（`begin_run` 重置）。
pub(crate) struct InteractionWindowManager {
    /// 进行中运行的 bridge。留在 UI 侧，好让排空循环能读进度、标记焦点已确认。
    bridge: InteractionBridge,
    /// 进行中运行的「脚本 → UI」请求流。
    rx: Receiver<UiRequest>,
    /// 与进行中运行共享的进度槽。
    progress: ProgressSlot,
    /// 当前运行的取消令牌：确认窗的「中止」按钮经它打断脚本侧的等待。
    cancel: CancelToken,
    console: ConsoleChannel,
    /// 待命 / 进度 / 确认窗，开着的时候才有值。
    window: Option<(AnyWindowHandle, Entity<InteractionWindow>)>,
    /// 等待作答的 `confirm`。
    pending_confirm: Option<(String, Sender<ConfirmChoice>)>,
    /// 等用户选窗口的 `requestFocus`。
    pending_focus: Option<(String, Sender<()>)>,
    /// 等原生选择器的 `pickPath`。
    pending_pick: Option<PickPending>,
    /// 待命窗是否至少被观察到一次处于激活态；只有此后「失去激活」才等于
    /// 「用户改选了别的窗口」。
    focus_activated: bool,
    /// 进度首次出现的时间，用于抑制窗口闪现。
    progress_since: Option<Instant>,
}

impl InteractionWindowManager {
    /// 空闲占位状态，首次运行时会被本轮自己的 bridge 替换掉。
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

    /// 排空交互请求，能就地解决的解决掉，并让交互窗口保持同步。运行期间
    /// 每个 tick 调一次。
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
                    // 原生选择器是模态的，没法在这里同步驱动，所以先发出去，
                    // 把 reply 记下来留到后续 tick 再收结果。
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

    /// 用户关掉原生选择器后，把结果转发回去。
    fn poll_pick(&mut self) {
        let Some((_, receiver)) = &mut self.pending_pick else {
            return;
        };
        let picked = match receiver.try_recv() {
            // 选择器还在屏幕上：继续等，下一个 tick 再看。
            Ok(None) => return,
            Ok(Some(Ok(paths))) => paths.and_then(|paths| paths.into_iter().next()),
            Ok(Some(Err(err))) => {
                self.console.warn(format!("Pick path failed: {err}"));
                None
            }
            // 发送端被丢弃：按「用户取消」处理，不当成失败。
            Err(oneshot::Canceled) => None,
        };
        if let Some((reply, _)) = self.pending_pick.take() {
            let _ = reply.send(picked);
        }
    }

    /// 用户把激活切到别的窗口后，释放焦点待命。
    fn poll_focus(&mut self, cx: &mut App) {
        if self.pending_focus.is_none() {
            return;
        }
        // 还没显示出来的窗口谈不上「失去激活」，所以必须先把窗口观察到一次
        // 激活态，释放逻辑才武装起来。
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

    /// 按当前待处理的事项决定交互窗口的开、关与内容更新。
    fn sync_window(&mut self, cx: &mut App) {
        let state = if let Some((message, _)) = &self.pending_confirm {
            InteractionState::Confirm {
                message: message.clone(),
            }
        } else if let Some((hint, _)) = &self.pending_focus {
            InteractionState::Focus { hint: hint.clone() }
        } else if let Some(snapshot) = self.progress.lock().clone() {
            // 短于延迟的进度不值得为它开个窗。
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

        // 创建时是否抢焦点。Focus/Confirm 必须抢：待命窗要靠失去激活来判断
        // 用户已切走，Confirm 还要接收按键；Progress 绝不能抢，否则模拟键入
        // 会打进我们自己的窗口。
        let wants_focus = !matches!(state, InteractionState::Progress { .. });

        let need_reopen = match &self.window {
            None => true,
            Some((_, view)) => {
                let current_state = view.read(cx).state();
                // 同一类状态：只更新内容即可，窗口保留现有焦点（同类切换时
                // 这正是想要的行为）。
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

    /// 打开置顶的交互窗口。Progress 状态必须传 `take_focus = false`，否则模拟
    /// 键入会落在这个浮窗上；Focus/Confirm 则要 `true`。
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

    /// 交互窗里的按钮在这里作答，保证每条 reply 通道只有一个所有者。
    /// 由 [`RunService`] 的订阅转发进来。
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
                // 先把中止原因交给脚本，再停掉本轮；令牌同时会打断该请求还在
                // 等待的 await。
                if let Some((_, reply)) = self.pending_confirm.take() {
                    let _ = reply.send(ConfirmChoice::Abort);
                }
                self.pending_focus = None;
                self.cancel.cancel();
            }
        }
    }
}
