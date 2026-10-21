//! The native JSON editor panel — uses gpui-kit's `Editor` with JSON syntax
//! highlighting + tree-sitter folding + line numbers, plus a row of format /
//! minify / escape / copy / clear tools. Keeps the "paste auto-format"
//! heuristic from the extension so pasting a blob of valid JSON immediately
//! gets pretty-printed.

use gpui_kit::assets::IconName;
use gpui_kit::component::ActiveTheme;
use gpui_kit::component::Sizable;
use gpui_kit::component::TitleBar;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Editor, EditorState, InputEvent, TabSize};
use gpui_kit::*;

use super::parsers;
use crate::ui::window_pin::WindowPin;

/// Net insertion of at least this many characters is treated as a paste —
/// auto-format the incoming text if it parses as strict JSON.
const PASTE_MIN: usize = 8;

#[derive(Clone, Debug)]
struct ToolButton {
    id: &'static str,
    icon: IconName,
    label: &'static str,
    action: ToolAction,
}

#[derive(Clone, Debug)]
enum ToolAction {
    Format,
    Minify,
    MinifyCopy,
    CopyRaw,
    Escape,
    Clear,
}

const TOOLS: &[ToolButton] = &[
    ToolButton {
        id: "fmt",
        icon: IconName::Braces,
        label: "重新格式化",
        action: ToolAction::Format,
    },
    ToolButton {
        id: "min",
        icon: IconName::Shrink,
        label: "压缩",
        action: ToolAction::Minify,
    },
    ToolButton {
        id: "min-copy",
        icon: IconName::ClipboardCopy,
        label: "压缩并复制",
        action: ToolAction::MinifyCopy,
    },
    ToolButton {
        id: "copy",
        icon: IconName::Copy,
        label: "复制原文",
        action: ToolAction::CopyRaw,
    },
    ToolButton {
        id: "esc",
        icon: IconName::Quote,
        label: "转义为字符串",
        action: ToolAction::Escape,
    },
    ToolButton {
        id: "clear",
        icon: IconName::Trash,
        label: "清空",
        action: ToolAction::Clear,
    },
];

pub struct JsonEditorPanel {
    focus_handle: FocusHandle,
    editor: Entity<EditorState>,
    /// 标题栏右上角的置顶图钉（公共控件，自带状态与窗口层级同步）。
    pin: Entity<WindowPin>,
    /// While `true`, a pending set_value on the editor is in flight — the
    /// corresponding `Change` subscription callback does nothing so we don't
    /// re-trigger auto-format on our own writes.
    suppress: bool,
    /// Net text we last observed from a `Change` event — used so we can
    /// compute whether the delta looks like a paste (large bulk insert).
    last_text: String,
    /// Latest status message to show in the bottom bar.
    status: String,
    /// A deferred write (e.g. paste auto-format) waiting for the next render
    /// cycle — `EditorState::set_value` needs a `&mut Window` which only
    /// render provides; subscribe callbacks do not.
    pending_set: Option<String>,
    _subscription: Subscription,
}

impl JsonEditorPanel {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let editor = cx.new(|ecx| {
            EditorState::new(window, ecx)
                .language("json")
                .line_number(true)
                .folding(true)
                .show_whitespaces(false)
                .tab_size(TabSize {
                    tab_size: 2,
                    hard_tabs: false,
                })
                .placeholder("粘贴或输入 JSON …")
        });

        let subscription = cx.subscribe(&editor, |this, _state, event: &InputEvent, cx| {
            if let InputEvent::Change = event {
                this.on_editor_change(cx);
            }
        });

        Self {
            focus_handle: cx.focus_handle(),
            editor,
            // 本窗口默认不置顶，与开窗时的窗口层级一致（unpinned）。
            pin: cx.new(|_| WindowPin::new(false)),
            suppress: false,
            last_text: String::new(),
            status: String::new(),
            pending_set: None,
            _subscription: subscription,
        }
    }

    fn on_editor_change(&mut self, cx: &mut Context<Self>) {
        if self.suppress {
            return;
        }
        let text = self.editor.read(cx).value().to_string();
        if text == self.last_text {
            return;
        }
        let inserted = text.len().saturating_sub(self.last_text.len());
        self.last_text = text.clone();

        if inserted >= PASTE_MIN {
            if let Ok(pretty) = parsers::format(&text) {
                // EditorState::set_value needs a &mut Window — render provides
                // one; subscribe callbacks do not. Defer to the next render
                // cycle by scheduling a notify and a `pending_set` field.
                self.pending_set = Some(pretty);
                self.status = "已自动格式化".to_string();
            } else {
                self.status.clear();
            }
        } else {
            self.status.clear();
        }
        cx.notify();
    }

    fn editor_text(&self, cx: &Context<Self>) -> String {
        self.editor.read(cx).value().to_string()
    }

    fn apply_pending(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(pending) = self.pending_set.take() {
            self.suppress = true;
            self.last_text = pending.clone();
            self.editor.update(cx, |state, cx| {
                state.set_value(&pending, window, cx);
            });
            self.suppress = false;
        }
    }

    fn run_tool(&mut self, action: &ToolAction, window: &mut Window, cx: &mut Context<Self>) {
        match action {
            ToolAction::Clear => {
                self.suppress = true;
                self.last_text.clear();
                self.editor.update(cx, |state, cx| {
                    state.set_value("", window, cx);
                });
                self.suppress = false;
                self.status.clear();
            }
            ToolAction::CopyRaw => {
                let text = self.editor_text(cx);
                cx.write_to_clipboard(text.into());
                self.status = "已复制原文".to_string();
            }
            ToolAction::Format => match parsers::format(&self.editor_text(cx)) {
                Ok(pretty) => {
                    self.write_back(window, cx, &pretty);
                    self.status = "已格式化".to_string();
                }
                Err(e) => {
                    self.status = format!("格式化失败：{e}");
                }
            },
            ToolAction::Minify => match parsers::minify(&self.editor_text(cx)) {
                Ok(min) => {
                    self.write_back(window, cx, &min);
                    self.status = "已压缩".to_string();
                }
                Err(e) => {
                    self.status = format!("压缩失败：{e}");
                }
            },
            ToolAction::MinifyCopy => match parsers::minify(&self.editor_text(cx)) {
                Ok(min) => {
                    cx.write_to_clipboard(min.clone().into());
                    self.status = "已复制压缩后的 JSON".to_string();
                }
                Err(e) => {
                    self.status = format!("压缩失败：{e}");
                }
            },
            ToolAction::Escape => {
                let escaped = parsers::escape(&self.editor_text(cx));
                self.write_back(window, cx, &escaped);
                self.status = "已转义为字符串".to_string();
            }
        }
        cx.notify();
    }

    fn write_back(&mut self, window: &mut Window, cx: &mut Context<Self>, text: &str) {
        self.suppress = true;
        self.last_text = text.to_string();
        self.editor.update(cx, |state, cx| {
            state.set_value(text, window, cx);
        });
        self.suppress = false;
    }
}

impl Focusable for JsonEditorPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for JsonEditorPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Apply any deferred writes (auto-format on paste) now that we have
        // a &mut Window — `set_value` requires it.
        self.apply_pending(window, cx);

        let colors = cx.theme().colors;

        gpui_kit::div()
            .flex()
            .flex_col()
            .size_full()
            .bg(colors.background)
            // 自绘标题栏：标题在左，置顶图钉在右上角（公共控件）。
            // 对应开窗时的 `TitleBar::window_options()` 基底。
            .child(
                TitleBar::new()
                    .child(
                        div()
                            .flex_shrink_0()
                            .text_size(px(13.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(colors.foreground)
                            .child("JSON 编辑器"),
                    )
                    .child(self.pin.clone()),
            )
            .child(
                Editor::new(&self.editor)
                    .flex_1()
                    // .p(px(12.))
                    .text_size(px(13.))
                    .text_color(colors.foreground)
                    .bg(colors.background),
            )
            .child(self.bottom_bar(cx))
    }
}

impl JsonEditorPanel {
    fn bottom_bar(&mut self, cx: &mut Context<Self>) -> gpui_kit::AnyElement {
        let colors = cx.theme().colors;
        let mut bar = div()
            .flex()
            .items_center()
            .gap(px(6.))
            .px(px(12.))
            .py(px(8.))
            .flex_shrink_0()
            .border_t_1()
            .border_color(colors.border);

        bar = bar.child(div().flex_1());

        if !self.status.is_empty() {
            bar = bar.child(
                div()
                    .text_size(px(11.))
                    .text_color(colors.muted_foreground)
                    .child(self.status.clone()),
            );
        }

        for tool in TOOLS {
            let action = tool.action.clone();
            bar = bar.child(
                Button::new(tool.id)
                    .ghost()
                    .icon(tool.icon)
                    .small()
                    .tooltip(tool.label)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.run_tool(&action, window, cx);
                    })),
            );
        }

        bar.into_any_element()
    }
}
