use std::time::Duration;

use crossbeam_channel::Receiver;
use gpui_kit::base::dock::PanelEvent;
use gpui_kit::component::Disableable;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::dock::{BasePanel, Panel};
use gpui_kit::component::scroll::ScrollableElement;
use gpui_kit::*;

use crate::console_channel::ConsoleEntry;

/// Maximum lines retained in the panel; older entries are dropped.
const MAX_ENTRIES: usize = 1000;

pub struct ConsolePanel {
    focus_handle: FocusHandle,
    rx: Receiver<ConsoleEntry>,
    entries: Vec<ConsoleEntry>,
    _task: Task<()>,
}

impl ConsolePanel {
    pub fn new(_window: &mut Window, rx: Receiver<ConsoleEntry>, cx: &mut Context<Self>) -> Self {
        let task = cx.spawn(Self::drain_loop);
        Self {
            focus_handle: cx.focus_handle(),
            rx,
            entries: Vec::new(),
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
        // Drain anything that arrived since the last render too, so typing in
        // the editor immediately surfaces pending output without waiting.
        while let Ok(entry) = self.rx.try_recv() {
            self.entries.push(entry);
        }
        if self.entries.len() > MAX_ENTRIES {
            let drop_n = self.entries.len() - MAX_ENTRIES;
            self.entries.drain(..drop_n);
        }

        let toolbar = div()
            .flex()
            .items_center()
            .justify_between()
            .px_3()
            .py_2()
            .border_b_1()
            .border_color(gpui::hsla(0., 0., 0.5, 0.2))
            .child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("Console"),
            )
            .child(
                Button::new("clear")
                    .label("Clear")
                    .ghost()
                    .compact()
                    .disabled(self.entries.is_empty())
                    .on_click(cx.listener(|this, _, _, cx| this.clear(cx))),
            );

        let mut list = div().flex().flex_col().py_1();
        if self.entries.is_empty() {
            list = list.child(
                div()
                    .p_4()
                    .text_xs()
                    .text_color(gpui::hsla(0., 0., 0.6, 1.))
                    .child("Console output will appear here."),
            );
        } else {
            for entry in self.entries.iter().rev() {
                let color = entry.level.color();
                let prefix = entry.level.prefix();
                let text = entry.text.clone();
                list = list.child(
                    div()
                        .px_3()
                        .py_0p5()
                        .text_xs()
                        .text_color(color)
                        .child(format!("{prefix}{text}")),
                );
            }
        }

        div()
            .size_full()
            .flex()
            .flex_col()
            .child(toolbar)
            .child(div().flex_1().min_h_0().child(list).overflow_y_scrollbar())
    }
}
