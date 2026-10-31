//! 常驻置顶窗口，承载运行中脚本的交互 UI。
//!
//! 它只渲染三种东西之一：等待提示（`requestFocus`）、是/否确认（`confirm`）
//! 或进度条（`typeStr` / `scanQr`）。它是一个纯视图：按钮只发出
//! [`InteractionEvent`]，回复通道归 workspace 所有，这样「是否已回答」只有
//! 一个地方在记。

use gpui_kit::component::ActiveTheme;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::progress::Progress;
use gpui_kit::{
    AnyElement, Context, EventEmitter, FontWeight, IntoElement, ParentElement, Render, Styled,
    Window, div,
};

use crate::runtime::interaction::ProgressSnapshot;

/// 窗口当前该显示什么。
#[derive(Clone, Debug, Default, PartialEq)]
pub enum InteractionState {
    /// 没有待办；此状态下 workspace 会把窗口关掉。
    #[default]
    Idle,
    /// 等待用户切走窗口（`requestFocus`）。
    Focus { hint: String },
    /// 是/否确认（`confirm`）。
    Confirm { message: String },
    /// 进度展示（`typeStr` / `scanQr`），`total` 为 0 表示总量未知。
    Progress { snapshot: ProgressSnapshot },
}

impl InteractionState {
    /// 处在该状态的窗口是否应该抢焦点。Progress 绝不能抢：否则模拟按键会落到
    /// 我们自己的浮层上；Focus 和 Confirm 需要焦点来触发 standby 释放和按钮点击。
    pub fn wants_focus(&self) -> bool {
        !matches!(
            self,
            InteractionState::Progress { .. } | InteractionState::Idle
        )
    }
}

/// 按钮发出这些事件；由 workspace 回相应答复 / 取消本次运行。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InteractionEvent {
    ConfirmYes,
    ConfirmNo,
    Abort,
}

/// 承载脚本交互 UI 的置顶窗口视图。
pub struct InteractionWindow {
    state: InteractionState,
}

impl InteractionWindow {
    /// 以 [`InteractionState::Idle`] 建视图。
    pub fn new(_cx: &mut Context<Self>) -> Self {
        Self {
            state: InteractionState::Idle,
        }
    }

    /// 换掉窗口显示的内容。状态没变时什么都不做，免得 50ms 的 drain 循环对
    /// 同一个状态反复重绘。
    pub fn set_state(&mut self, state: InteractionState, cx: &mut Context<Self>) {
        if self.state != state {
            self.state = state;
            cx.notify();
        }
    }

    /// 当前状态（只读），workspace 据此决定是否聚焦、何时关窗。
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

                // 速度：每秒字符数。用 0ms 兜底，避免第一个 tick 结算前除零。
                let speed = if snapshot.current > 0 && snapshot.elapsed_ms > 0 {
                    let chars_per_sec =
                        snapshot.current as f64 / (snapshot.elapsed_ms as f64 / 1000.0);
                    format!("{:.1}字符/秒", chars_per_sec)
                } else {
                    String::new()
                };

                // 已用时间：始终精确到秒，用户才看得到秒数在跳。
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

                // 顶行：标题（左）· 百分比（右）
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

                // 中间：进度条
                let bar = Progress::new("interaction-progress")
                    .value(percent)
                    .loading(snapshot.total == 0);

                // 底行：计数 + 速度（左）· 耗时预估（右）
                let bottom_right = match (total_est, remaining) {
                    (Some(total), Some(rem)) => {
                        format!("{} / {} ~ {}", elapsed, total, rem)
                    }
                    _ => elapsed.to_string(),
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
        // 中止按钮用 Lucide 全量图标集（assets::IconName）：组件自带的枚举只有
        // 一个固定的兼容子集，没有 SquareStop。
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

/// 把毫秒数格式化为 `X小时X分X秒`，秒永远显示。
///
/// 用于「已用」计时，它必须看得出在跳。
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

/// 把毫秒数格式化为 `X小时X分`。
///
/// 总量 ≥ 60 秒时省掉末位的秒，避免 UI 抖动——剩多少 / 总共多久的估算在均值
/// 收敛过程中会来回摆几秒，长任务上一个翻动的秒位很干扰。少于 60 秒仍然显示秒。
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
