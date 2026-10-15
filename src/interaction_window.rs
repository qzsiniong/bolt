//! Always-on-top window that hosts the interaction UI for a running script.
//!
//! It renders one of three things: a standby notice (`requestFocus`), a yes/no
//! question (`confirm`) or a progress bar (`typeStr` / `scanQr`). It is a pure
//! view: buttons only emit [`InteractionEvent`], and the workspace owns the
//! reply channels, so "answered" is tracked in exactly one place.

use gpui_kit::component::ActiveTheme;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::progress::Progress;
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;

use crate::interaction::ProgressSnapshot;

/// What the window should show right now.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum InteractionState {
    /// Nothing pending; the workspace closes the window in this state.
    #[default]
    Idle,
    Focus {
        hint: String,
    },
    Confirm {
        message: String,
    },
    Progress {
        snapshot: ProgressSnapshot,
    },
}

impl InteractionState {
    /// Whether a window in this state should grab focus. Progress must NOT
    /// steal focus or simulated keystrokes land on our own overlay; Focus and
    /// Confirm need focus to drive standby-release and button clicks.
    pub fn wants_focus(&self) -> bool {
        !matches!(
            self,
            InteractionState::Progress { .. } | InteractionState::Idle
        )
    }
}

/// Buttons emit these; the workspace sends the matching reply / cancels the run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InteractionEvent {
    ConfirmYes,
    ConfirmNo,
    Abort,
}

pub struct InteractionWindow {
    state: InteractionState,
}

impl InteractionWindow {
    pub fn new(_cx: &mut Context<Self>) -> Self {
        Self {
            state: InteractionState::Idle,
        }
    }

    /// Replace what the window shows. No-op when unchanged, so the 50ms drain
    /// loop does not repaint the same state over and over.
    pub fn set_state(&mut self, state: InteractionState, cx: &mut Context<Self>) {
        if self.state != state {
            self.state = state;
            cx.notify();
        }
    }

    pub fn state(&self) -> &InteractionState {
        &self.state
    }

    fn emit(&self, event: InteractionEvent, cx: &mut Context<Self>) {
        cx.emit(event);
    }

    fn header(&self, cx: &mut Context<Self>) -> AnyElement {
        let (title, muted) = match &self.state {
            InteractionState::Idle | InteractionState::Progress { .. } => {
                return div().into_any_element();
            }
            InteractionState::Focus { .. } => ("等待切换窗口".to_string(), true),
            InteractionState::Confirm { .. } => ("确认".to_string(), true),
        };
        div()
            .text_sm()
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(if muted {
                cx.theme().muted_foreground
            } else {
                cx.theme().foreground
            })
            .child(title)
            .into_any_element()
    }

    fn body(&self, cx: &mut Context<Self>) -> AnyElement {
        match &self.state {
            InteractionState::Idle => div().into_any_element(),
            InteractionState::Focus { hint } => div()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().foreground)
                        .child(hint.clone()),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child("本窗口失去焦点后自动继续"),
                )
                .into_any_element(),
            InteractionState::Confirm { message } => div()
                .text_sm()
                .text_color(cx.theme().foreground)
                .child(message.clone())
                .into_any_element(),
            InteractionState::Progress { snapshot } => {
                let percent = if snapshot.total == 0 {
                    0.0
                } else {
                    (snapshot.current as f32 / snapshot.total as f32 * 100.0).clamp(0.0, 100.0)
                };
                let percent_str = format!("{:.0}%", percent);

                // Speed: chars per second. 0ms guard avoids div-by-zero before
                // the first tick is settled.
                let speed = if snapshot.current > 0 && snapshot.elapsed_ms > 0 {
                    let chars_per_sec =
                        snapshot.current as f64 / (snapshot.elapsed_ms as f64 / 1000.0);
                    format!("{:.1}字符/秒", chars_per_sec)
                } else {
                    String::new()
                };

                // Elapsed: always show seconds so the user sees the clock tick.
                let elapsed = human_duration_precise(snapshot.elapsed_ms);
                let remaining = if snapshot.total == 0 || snapshot.current == 0 {
                    None
                } else {
                    let per_unit = snapshot.elapsed_ms as f64 / snapshot.current as f64;
                    let remaining_ms =
                        (per_unit * (snapshot.total - snapshot.current) as f64) as u64;
                    Some(human_duration_eta(remaining_ms))
                };
                let total_est = if snapshot.total == 0 || snapshot.current == 0 {
                    None
                } else {
                    let per_unit = snapshot.elapsed_ms as f64 / snapshot.current as f64;
                    let total_ms = (per_unit * snapshot.total as f64) as u64;
                    Some(human_duration_eta(total_ms))
                };

                // Top row: label (left) · percent (right)
                let top = div()
                    .flex()
                    .justify_between()
                    .items_center()
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(cx.theme().foreground)
                            .child(snapshot.label.clone()),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(percent_str),
                    );

                // Middle: progress bar
                let bar = Progress::new("interaction-progress")
                    .value(percent)
                    .loading(snapshot.total == 0);

                // Bottom row: count + speed (left) · ETA info (right)
                let bottom_right = match (total_est, remaining) {
                    (Some(total), Some(rem)) => {
                        format!("{} / {} ~ {}", elapsed, total, rem)
                    }
                    _ => format!("{}", elapsed),
                };
                let mut bottom_left = div()
                    .flex()
                    .gap_2()
                    .items_center()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(format!("{}/{}", snapshot.current, snapshot.total));
                if !speed.is_empty() {
                    bottom_left = bottom_left.child(speed);
                }
                let bottom = div()
                    .flex()
                    .justify_between()
                    .items_center()
                    .child(bottom_left)
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(bottom_right),
                    );

                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(top)
                    .child(bar)
                    .child(bottom)
                    .into_any_element()
            }
        }
    }

    fn buttons(&self, cx: &mut Context<Self>) -> AnyElement {
        // Stop uses the full Lucide catalog (assets::IconName); the component
        // enum only carries a fixed compatibility subset and has no SquareStop.
        let abort = Button::new("abort")
            .icon(gpui_kit::assets::IconName::SquareStop)
            .label("中止")
            .danger()
            .compact()
            .tooltip("中止脚本")
            .on_click(cx.listener(|this, _, _, cx| this.emit(InteractionEvent::Abort, cx)));

        match &self.state {
            InteractionState::Idle => div().into_any_element(),
            InteractionState::Focus { .. } | InteractionState::Progress { .. } => {
                div().flex().justify_end().child(abort).into_any_element()
            }
            InteractionState::Confirm { .. } => {
                div()
                    .flex()
                    .gap_2()
                    .justify_end()
                    .child(
                        Button::new("confirm-no")
                            .label("否")
                            .ghost()
                            .compact()
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.emit(InteractionEvent::ConfirmNo, cx)
                            })),
                    )
                    .child(
                        Button::new("confirm-yes")
                            .label("是")
                            .primary()
                            .compact()
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.emit(InteractionEvent::ConfirmYes, cx)
                            })),
                    )
                    .child(abort)
                    .into_any_element()
            }
        }
    }
}

impl EventEmitter<InteractionEvent> for InteractionWindow {}

/// Format a millisecond count as `X小时X分X秒`, always showing seconds.
///
/// Used for the "已用" timer, which should visibly tick.
fn human_duration_precise(ms: u64) -> String {
    let total_secs = ms / 1000;
    let h = total_secs / 3600;
    let m = (total_secs % 3600) / 60;
    let s = total_secs % 60;
    if h > 0 {
        format!("{h}小时{m}分{s}秒")
    } else if m > 0 {
        format!("{m}分{s}秒")
    } else {
        format!("{s}秒")
    }
}

/// Format a millisecond count as `X小时X分`.
///
/// When the total is >= 60 seconds the trailing unit (seconds) is omitted to
/// avoid UI jitter — remaining / total estimates fluctuate by a few seconds as
/// the average settles, and a 1-second digit flipping is distracting on long
/// operations. Below 60 seconds seconds are still shown.
fn human_duration_eta(ms: u64) -> String {
    let total_secs = ms / 1000;
    let h = total_secs / 3600;
    let m = (total_secs % 3600) / 60;
    let s = total_secs % 60;
    if h > 0 {
        if total_secs >= 60 {
            format!("{h}小时{m}分")
        } else {
            format!("{h}小时{m}分{s}秒")
        }
    } else if total_secs >= 60 {
        format!("{m}分")
    } else if m > 0 {
        format!("{m}分{s}秒")
    } else {
        format!("{s}秒")
    }
}

impl Render for InteractionWindow {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .gap_3()
            .size_full()
            .p_4()
            .child(self.header(cx))
            .child(self.body(cx))
            .child(self.buttons(cx))
    }
}
