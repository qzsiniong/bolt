use std::time::Duration;

use crossbeam_channel::Receiver;
use gpui_kit::base::dock::PanelEvent;
use gpui_kit::component::ActiveTheme;
use gpui_kit::component::Disableable;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::dock::{BasePanel, Panel};
use gpui_kit::component::scroll::ScrollableElement;
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;

use crate::console_channel::{ConsoleEntry, ConsoleLevel};

/// Maximum lines retained in the panel; older entries are dropped.
const MAX_ENTRIES: usize = 1000;

pub struct ConsolePanel {
    focus_handle: FocusHandle,
    rx: Receiver<ConsoleEntry>,
    entries: Vec<ConsoleEntry>,
    scroll_handle: ScrollHandle,
    /// `None` shows every level; `Some(level)` shows only that level.
    filter: Option<ConsoleLevel>,
    filter_menu_open: bool,
    _task: Task<()>,
}

impl ConsolePanel {
    pub fn new(_window: &mut Window, rx: Receiver<ConsoleEntry>, cx: &mut Context<Self>) -> Self {
        let task = cx.spawn(Self::drain_loop);
        Self {
            focus_handle: cx.focus_handle(),
            rx,
            entries: Vec::new(),
            scroll_handle: ScrollHandle::new(),
            filter: None,
            filter_menu_open: false,
            _task: task,
        }
    }

    /// Background loop: drain the channel a few times a second and trigger a
    /// re-render whenever new lines arrive. Exits when the panel is dropped.
    async fn drain_loop(this: WeakEntity<Self>, cx: &mut AsyncApp) {
        loop {
            cx.background_executor()
                .timer(Duration::from_millis(80))
                .await;
            let alive = this
                .update(cx, |panel, cx| {
                    let mut drained = false;
                    while let Ok(entry) = panel.rx.try_recv() {
                        panel.entries.push(entry);
                        drained = true;
                    }
                    if panel.entries.len() > MAX_ENTRIES {
                        let drop_n = panel.entries.len() - MAX_ENTRIES;
                        panel.entries.drain(..drop_n);
                    }
                    if drained {
                        cx.notify();
                    }
                })
                .is_ok();
            if !alive {
                break;
            }
        }
    }

    fn clear(&mut self, cx: &mut Context<Self>) {
        self.entries.clear();
        cx.notify();
    }

    fn set_filter(&mut self, filter: Option<ConsoleLevel>, cx: &mut Context<Self>) {
        self.filter = filter;
        self.filter_menu_open = false;
        cx.notify();
    }
}

/// Format an epoch-millis timestamp as local `HH:MM:SS`.
fn format_time(timestamp: i64) -> String {
    let secs = timestamp / 1000;
    let local = (secs.rem_euclid(86_400) + local_utc_offset_secs()).rem_euclid(86_400);
    format!(
        "{:02}:{:02}:{:02}",
        local / 3600,
        (local % 3600) / 60,
        local % 60
    )
}

/// Local UTC offset in seconds, resolved once via `date +%z` (std has no
/// timezone API). Falls back to 0 (UTC) when unavailable.
fn local_utc_offset_secs() -> i64 {
    use std::sync::OnceLock;
    static OFFSET: OnceLock<i64> = OnceLock::new();
    *OFFSET.get_or_init(|| {
        std::process::Command::new("date")
            .arg("+%z")
            .output()
            .ok()
            .and_then(|out| String::from_utf8(out.stdout).ok())
            .and_then(|s| {
                let s = s.trim().to_string();
                let sign = if s.starts_with('-') { -1 } else { 1 };
                let digits = s.trim_start_matches(['+', '-']);
                let hours: i64 = digits.get(0..2)?.parse().ok()?;
                let mins: i64 = digits.get(2..4)?.parse().ok()?;
                Some(sign * (hours * 3600 + mins * 60))
            })
            .unwrap_or(0)
    })
}

fn level_color(level: ConsoleLevel, cx: &App) -> Hsla {
    let theme = cx.theme();
    match level {
        ConsoleLevel::Log => theme.foreground,
        ConsoleLevel::Info => theme.info,
        ConsoleLevel::Warn => theme.warning,
        ConsoleLevel::Error => theme.danger,
    }
}

impl BasePanel for ConsolePanel {
    fn panel_name(&self) -> &'static str {
        "Console"
    }
}

impl Panel for ConsolePanel {
    fn title(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let count = self.entries.len();
        let label = if count == 0 {
            "Console".to_string()
        } else {
            format!("Console ({count})")
        };
        div().text_sm().child(label).into_any_element()
    }
}

impl EventEmitter<PanelEvent> for ConsolePanel {}

impl Focusable for ConsolePanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for ConsolePanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Follow mode: auto-scroll to the newest entry as long as the user
        // hasn't scrolled away from the bottom (checked against the previous
        // frame's scroll state, before new entries shift the content).
        let at_bottom = self.scroll_handle.offset().y >= self.scroll_handle.max_offset().y - px(4.);

        // Drain anything that arrived since the last render too, so typing in
        // the editor immediately surfaces pending output without waiting.
        while let Ok(entry) = self.rx.try_recv() {
            self.entries.push(entry);
        }
        if self.entries.len() > MAX_ENTRIES {
            let drop_n = self.entries.len() - MAX_ENTRIES;
            self.entries.drain(..drop_n);
        }

        let theme = cx.theme();
        let border = theme.border;
        let muted_fg = theme.muted_foreground;
        let accent = theme.accent;

        // Level filter dropdown trigger + popup.
        let filter_label = self.filter.map(ConsoleLevel::label).unwrap_or("全部等级");
        let mut filter_menu = div().flex().flex_col().py_1();
        let options: Vec<(Option<ConsoleLevel>, &str)> = std::iter::once((None, "全部等级"))
            .chain(ConsoleLevel::ALL.iter().map(|l| (Some(*l), l.label())))
            .collect();
        for (opt_idx, (value, label)) in options.into_iter().enumerate() {
            let selected = self.filter == value;
            filter_menu = filter_menu.child(
                div()
                    .id(("filter-opt", opt_idx))
                    .px_3()
                    .py_1()
                    .text_xs()
                    .when(selected, |s| s.bg(accent.opacity(0.15)))
                    .hover(|s| s.bg(muted_fg.opacity(0.15)).cursor_pointer())
                    .child(label)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.set_filter(value, cx);
                    })),
            );
        }

        let toolbar = div()
            .flex()
            .items_center()
            .justify_between()
            .px_3()
            .py_2()
            .border_b_1()
            .border_color(border)
            .child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("Console"),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .relative()
                            .child(
                                Button::new("filter")
                                    .label(filter_label)
                                    .icon(gpui_kit::component::IconName::ChevronDown)
                                    .ghost()
                                    .compact()
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.filter_menu_open = !this.filter_menu_open;
                                        cx.notify();
                                    })),
                            )
                            .when(self.filter_menu_open, |s| {
                                s.child(
                                    div()
                                        .absolute()
                                        .top_full()
                                        .right_0()
                                        .mt_1()
                                        .min_w(px(96.))
                                        .rounded_md()
                                        .border_1()
                                        .border_color(border)
                                        .bg(theme.popover)
                                        .shadow_md()
                                        .child(filter_menu),
                                )
                            }),
                    )
                    .child(
                        Button::new("clear")
                            .label("Clear")
                            .ghost()
                            .compact()
                            .disabled(self.entries.is_empty())
                            .on_click(cx.listener(|this, _, _, cx| this.clear(cx))),
                    ),
            );

        // Oldest entries first, newest at the bottom (like a terminal).
        let mut list = div().flex().flex_col().py_1();
        let filtered: Vec<&ConsoleEntry> = self
            .entries
            .iter()
            .filter(|e| self.filter.map(|f| f == e.level).unwrap_or(true))
            .collect();
        if filtered.is_empty() {
            list = list.child(
                div()
                    .p_4()
                    .text_xs()
                    .text_color(muted_fg)
                    .child("Console output will appear here."),
            );
        } else {
            for entry in filtered {
                let color = level_color(entry.level, cx);
                let prefix = entry.level.prefix();
                let time = format_time(entry.timestamp);
                let text = entry.text.clone();
                list = list.child(
                    div()
                        .flex()
                        .gap_2()
                        .px_3()
                        .py_0p5()
                        .text_xs()
                        .child(div().text_color(muted_fg).child(time))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_color(color)
                                .child(format!("{prefix}{text}")),
                        ),
                );
            }
        }

        if at_bottom {
            self.scroll_handle.scroll_to_bottom();
        }

        div().size_full().flex().flex_col().child(toolbar).child(
            div()
                .id("console-list")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .track_scroll(&self.scroll_handle)
                .vertical_scrollbar(&self.scroll_handle)
                .child(list),
        )
    }
}
