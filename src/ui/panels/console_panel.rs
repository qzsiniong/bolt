use std::time::Duration;

use crossbeam_channel::Receiver;
use gpui_kit::HighlightStyle;
use gpui_kit::base::dock::PanelEvent;
use gpui_kit::component::ActiveTheme;
use gpui_kit::component::Disableable;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::dock::{BasePanel, Panel};
use gpui_kit::component::input::{Editor, EditorState, TextDecoration, TextDecorationCollection};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;

use crate::console_channel::{ConsoleEntry, ConsoleLevel};

/// Maximum lines retained in the panel; older entries are dropped.
const MAX_ENTRIES: usize = 1000;

pub struct ConsolePanel {
    focus_handle: FocusHandle,
    rx: Receiver<ConsoleEntry>,
    entries: Vec<ConsoleEntry>,
    editor: Entity<EditorState>,
    decorations: TextDecorationCollection,
    /// Cached last rendered text to skip redundant set_value calls.
    last_text: String,
    /// Whether the viewport follows new output. Cleared when the user scrolls
    /// away from the bottom, restored when the bottom comes back into view.
    follow: bool,
    /// `None` shows every level; `Some(level)` shows only that level.
    filter: Option<ConsoleLevel>,
    filter_menu_open: bool,
    _task: Task<()>,
}

impl ConsolePanel {
    pub fn new(window: &mut Window, rx: Receiver<ConsoleEntry>, cx: &mut Context<Self>) -> Self {
        let task = cx.spawn(Self::drain_loop);
        let editor = cx.new(|ecx| EditorState::new(window, ecx));
        editor.update(cx, |s, cx| s.set_readonly(true, cx));
        let decorations = editor.update(cx, |s, cx| s.create_decorations_collection(vec![], cx));
        Self {
            focus_handle: cx.focus_handle(),
            rx,
            entries: Vec::new(),
            editor,
            decorations,
            last_text: String::new(),
            follow: true,
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
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
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

        // Build the full text and a color decoration for each entry's byte
        // range. A single readonly Editor gives native selection/copy, while
        // TextDecoration keeps the per-level color coding.
        let filtered: Vec<&ConsoleEntry> = self
            .entries
            .iter()
            .filter(|e| self.filter.map(|f| f == e.level).unwrap_or(true))
            .collect();

        let mut full_text = String::new();
        let mut decorations = Vec::with_capacity(filtered.len());
        for entry in filtered {
            let line = format!(
                "{} {}{}",
                format_time(entry.timestamp),
                entry.level.prefix(),
                entry.text
            );
            let start = full_text.len();
            full_text.push_str(&line);
            let end = full_text.len();
            full_text.push('\n');
            decorations.push(TextDecoration::new(
                start..end,
                HighlightStyle {
                    color: Some(level_color(entry.level, cx)),
                    ..Default::default()
                },
            ));
        }
        // Strip the trailing newline so copied text ends cleanly.
        if full_text.ends_with('\n') {
            full_text.pop();
        }

        if full_text != self.last_text {
            // `self.last_text` still matches what the editor holds, so its line
            // count (counted the way the rope counts lines, where a trailing
            // newline opens a new, empty line) can be compared against the
            // viewport of the last painted frame: the view follows output only
            // while that frame was at the bottom, which also resumes following
            // when the user scrolls back down.
            let total_lines = if self.last_text.is_empty() {
                0
            } else {
                self.last_text.matches('\n').count() + 1
            };
            self.follow = match self.editor.read(cx).visible_row_range() {
                Some(range) => range.end >= total_lines,
                // No layout yet: nothing has been scrolled away from.
                None => true,
            };

            self.last_text = full_text.clone();
            let text_len = full_text.len();
            let follow = self.follow;
            self.editor.update(cx, |s, cx| {
                let previous_offset = s.scroll_offset();
                s.set_value(&full_text, window, cx);
                // Place the cursor at the end so the editor scrolls to the
                // newest output. set_selected_range does not steal focus.
                s.set_selected_range(text_len..text_len, cx);
                if !follow {
                    // The user is reading older output: set_value resets the
                    // scroll to the top and the caret-follow above scrolls to
                    // the end, so the deferred offset is what puts the viewport
                    // back where the user left it.
                    s.set_scroll_offset(previous_offset, cx);
                }
            });
            self.decorations.set(decorations, cx);
        }

        div().size_full().flex().flex_col().child(toolbar).child(
            div().flex_1().min_h_0().child(
                Editor::new(&self.editor)
                    .readonly(true)
                    .appearance(false)
                    .bordered(false)
                    .size_full(),
            ),
        )
    }
}
