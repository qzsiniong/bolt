//! Console 视图：脚本管理器 dock 里的面板，也是独立 Console 窗口的根视图。
//!
//! 两种形态共用同一个组件（`chrome` 区分）：dock 里由 dock 画标题栏，独立窗口
//! 要自己画一条 `TitleBar`。内容一律从 [`ConsoleLog`] 按游标增量取——面板和窗口
//! 可以同时开着，各看各的滚动位置，谁也不抢谁的输出。

use std::time::Duration;

use gpui_kit::HighlightStyle;
use gpui_kit::base::dock::PanelEvent;
use gpui_kit::component::ActiveTheme;
use gpui_kit::component::Disableable;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::dock::{BasePanel, Panel};
use gpui_kit::component::input::{Editor, EditorState, TextDecoration, TextDecorationCollection};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::{
    AnyElement, App, AppContext, AsyncApp, Context, Entity, EventEmitter, FocusHandle, Focusable,
    FontWeight, Hsla, InteractiveElement, IntoElement, ParentElement, Render,
    StatefulInteractiveElement, Styled, Task, WeakEntity, Window, div, px,
};

use crate::console_channel::{ConsoleEntry, ConsoleLevel, ConsoleLog};
use crate::ui::window::{WindowPin, WindowTitleBar};

/// Maximum lines retained in the view; older entries are dropped.
const MAX_ENTRIES: usize = 1000;

/// 这个视图是被 dock 嵌着，还是自己占一个窗口。
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Chrome {
    Docked,
    Windowed,
}

pub(crate) struct ConsolePanel {
    focus_handle: FocusHandle,
    log: ConsoleLog,
    /// 已经消费到 `log` 的哪个位置。每个视图各一份，互不干扰。
    cursor: u64,
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
    chrome: Chrome,
    /// 独立窗口标题栏右上角的置顶图钉（公共控件，自带状态与窗口层级同步）。
    /// dock 形态没有自己的标题栏，也就没有图钉。
    pin: Option<Entity<WindowPin>>,
    _task: Task<()>,
}

impl ConsolePanel {
    /// dock 面板形态（脚本管理器窗口右下角）。
    pub(crate) fn new(window: &mut Window, log: ConsoleLog, cx: &mut Context<Self>) -> Self {
        Self::with_chrome(window, log, Chrome::Docked, cx)
    }

    /// 独立窗口形态（Launcher →「Console」）。
    pub(crate) fn new_windowed(
        window: &mut Window,
        log: ConsoleLog,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::with_chrome(window, log, Chrome::Windowed, cx)
    }

    fn with_chrome(
        window: &mut Window,
        log: ConsoleLog,
        chrome: Chrome,
        cx: &mut Context<Self>,
    ) -> Self {
        let task = cx.spawn(Self::drain_loop);
        let editor = cx.new(|ecx| EditorState::new(window, ecx));
        editor.update(cx, |s, cx| s.set_readonly(true, cx));
        let decorations = editor.update(cx, |s, cx| s.create_decorations_collection(vec![], cx));
        // 从头读：窗口/面板晚打开时也能看到启动期与之前的输出（缓冲只留最近
        // 1000 条，更早的已经被裁掉）。之后按游标取增量。
        let entries = log.entries();
        let cursor = log.revision();
        // 独立窗口有标题栏与置顶图钉：图钉无状态、读窗口真实层级，开窗侧
        // （`WindowSpec.pinned(true)`）已在创建视图之前把层级抬到 floating。
        // dock 面板跟着管理器窗口走，没有自己的层级，也就不创建图钉。
        let windowed = chrome == Chrome::Windowed;
        let pin = windowed.then(|| cx.new(|_| WindowPin::new()));
        Self {
            focus_handle: cx.focus_handle(),
            log,
            cursor,
            entries,
            editor,
            decorations,
            last_text: String::new(),
            follow: true,
            filter: None,
            filter_menu_open: false,
            chrome,
            pin,
            _task: task,
        }
    }

    /// 取走缓冲里的新条目；返回是否有变化。
    fn poll(&mut self) -> bool {
        let (incoming, cursor) = self.log.since(self.cursor);
        self.cursor = cursor;
        if incoming.is_empty() {
            return false;
        }
        self.entries.extend(incoming);
        if self.entries.len() > MAX_ENTRIES {
            let drop_n = self.entries.len() - MAX_ENTRIES;
            self.entries.drain(..drop_n);
        }
        true
    }

    /// Background loop: poll the shared buffer a few times a second and trigger
    /// a re-render whenever new lines arrive. Exits when the view is dropped.
    async fn drain_loop(this: WeakEntity<Self>, cx: &mut AsyncApp) {
        loop {
            cx.background_executor()
                .timer(Duration::from_millis(80))
                .await;
            let alive = this
                .update(cx, |panel, cx| {
                    if panel.poll() {
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

    /// 独立窗口的标题栏（dock 形态由 dock 自己画标签，用不到）。
    fn header(&self) -> AnyElement {
        let count = self.entries.len();
        let mut bar = WindowTitleBar::new("Console").subtitle(format!("{count} 条输出"));
        if let Some(pin) = self.pin.clone() {
            bar = bar.pin(pin);
        }
        bar.into_any_element()
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
        // Drain anything that arrived since the last poll too, so typing in
        // the editor immediately surfaces pending output without waiting.
        self.poll();

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

        // dock 形态下 dock 标签已经写着 "Console"，这里再写一遍只是重复；
        // 独立窗口的左半边留给筛选按钮。
        let toolbar_left = match self.chrome {
            Chrome::Docked => div()
                .text_sm()
                .font_weight(FontWeight::SEMIBOLD)
                .child("Console"),
            Chrome::Windowed => div(),
        };

        let toolbar = div()
            .flex()
            .items_center()
            .justify_between()
            .px_3()
            .py_2()
            .border_b_1()
            .border_color(border)
            .child(toolbar_left)
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

        let mut root = div().size_full().flex().flex_col();
        if self.chrome == Chrome::Windowed {
            root = root.child(self.header());
        }
        root.child(toolbar)
            .child(
                div().flex_1().min_h_0().child(
                    Editor::new(&self.editor)
                        .readonly(true)
                        .appearance(false)
                        .bordered(false)
                        .size_full(),
                ),
            )
            .into_any_element()
    }
}
