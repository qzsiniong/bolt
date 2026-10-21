//! Pin 便签窗口视图：无边框透明 Floating，内容区域支持拖拽 / 双击关闭，
//! 以及右上角自绘的小操作菜单（悬停显示）。

use std::sync::Arc;

use gpui_kit::assets::IconName;
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::Sizable;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::clipboard::{self, PinContent};
use crate::ui::window_pin::set_window_level;

/// 悬停操作菜单的右上角小按钮的图标名 / 提示语 / 点击回调。
#[derive(Clone, Debug)]
enum PanelAction {
    Close,
    Copy,
    TogglePin,
    Save,
}

pub struct PinWindow {
    content: PinContent,
    /// 是否处于悬浮态（鼠标在窗口内），控制右上角操作菜单显隐。
    hovered: bool,
    /// 是否已经置顶。运行时用户可点右上角图标切换。
    pinned: bool,
}

impl PinWindow {
    pub(crate) fn new(content: PinContent, _window: &mut Window, _cx: &mut Context<Self>) -> Self {
        Self {
            content,
            hovered: false,
            pinned: true,
        }
    }

    fn on_drag_down(&mut self, event: &MouseDownEvent, window: &mut Window) {
        // 双击直接关闭；否则让 AppKit 接管窗口拖动。
        if event.click_count >= 2 {
            window.remove_window();
            return;
        }
        window.start_window_move();
    }

    fn run_action(&mut self, action: &PanelAction, window: &mut Window, cx: &mut Context<Self>) {
        match action {
            PanelAction::Close => window.remove_window(),
            PanelAction::Copy => self.copy_to_clipboard(cx),
            PanelAction::TogglePin => {
                self.pinned = !self.pinned;
                set_window_level(window, self.pinned);
            }
            PanelAction::Save => self.save_to_disk(cx),
        }
        cx.notify();
    }

    fn copy_to_clipboard(&self, cx: &Context<Self>) {
        match &self.content {
            PinContent::Image { bytes, .. } => {
                // 便签里的图片字节已经是 PNG（TIFF 等格式在读取时就转过了），
                // 直接按 PNG 塞回剪切板；GPUI 的 ClipboardItem 没有 From<Vec<u8>>。
                let image = Image::from_bytes(ImageFormat::Png, bytes.clone());
                cx.write_to_clipboard(image.into());
            }
            PinContent::Text(text) => cx.write_to_clipboard(text.clone().into()),
        }
    }

    fn save_to_disk(&self, cx: &Context<Self>) {
        let dir = clipboard::default_save_dir();
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let path = match &self.content {
            PinContent::Image { .. } => dir.join(format!("pin-{ts}.png")),
            PinContent::Text(_) => dir.join(format!("pin-{ts}.txt")),
        };
        let result = match &self.content {
            PinContent::Image { bytes, .. } => std::fs::write(&path, bytes),
            PinContent::Text(text) => std::fs::write(&path, text),
        };
        let console = cx
            .global::<crate::app::shell::AppShell>()
            .console_channel
            .clone();
        match result {
            Ok(()) => console.info(format!("已保存到 {}", path.display())),
            Err(e) => console.error(format!("保存失败 {e}")),
        }
    }
}

impl Render for PinWindow {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors;

        // 根容器：背景跟主题保持一致，内部再按内容类型给自己的底。
        // 需要 `.id()`：`on_hover` 是 `StatefulInteractiveElement` 的方法，
        // 只挂在 `.id()` 之后得到的 `Stateful<Div>` 上（普通 `Div` 上只有
        // 固有的 `on_mouse_down` / `on_mouse_exit`）。
        let mut root = gpui_kit::div()
            .id("pin-root")
            .flex()
            .flex_col()
            .size_full()
            .bg(colors.background)
            // 整窗都可以用来拖拽。内部小按钮会 stop_propagation。
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event, window, _cx| {
                    this.on_drag_down(event, window);
                }),
            )
            // `on_hover` 在进入和离开时都会回调，用它统一驱动 hovered。
            .on_hover({
                let this = cx.entity().downgrade();
                move |hovered: &bool, _window, cx| {
                    let hovered = *hovered;
                    let _ = this.update(cx, |this, cx| {
                        if this.hovered != hovered {
                            this.hovered = hovered;
                            cx.notify();
                        }
                    });
                }
            });

        // 右上角操作菜单（悬停时浮现）。
        let hover_buttons = if self.hovered {
            Some(self.action_buttons(cx))
        } else {
            None
        };

        // 内容区域：按类型渲染。
        let content_el = match &self.content {
            PinContent::Image { bytes, .. } => {
                // 不能走 `img("data:image/png;base64,…")`：那个字符串会被
                // `url::Url::from_str` 判成合法 URI，进 `Resource::Uri` 去 fetch
                // 资源，取不到就**静默**渲染成空白。gpui 也没有任何 `data:`
                // 分支。可行的是内存图元 `ImageSource::Image(Arc<Image>)`
                // （`Arc<Image>` 有 `From`），它由 `AssetLogger<ImageDecoder>`
                // 直接从字节解码。
                //
                // 尺寸必须显式给：`img` 的固有尺寸只在解码完成后的那一帧才有，
                // 首帧它是 `Auto`，只给 `max_w/max_h` 会塌成 0×0（窗口全空）。
                // 用窗口 bounds 的同一份 `display_size()` 就没这个问题。
                let (w, h) = self.content.display_size();
                let image = Arc::new(Image::from_bytes(ImageFormat::Png, bytes.clone()));
                div()
                    .child(gpui_kit::img(image).w(px(w)).h(px(h)).rounded(px(4.)))
                    .into_any_element()
            }
            PinContent::Text(text) => div()
                .p(px(12.))
                .rounded(px(8.))
                .bg(hsla(0., 0., 0., 0.78))
                // `text_color` / `text_size` 是 Div（Styled）上的方法，不是
                // `gpui_kit::text!` 宏产物的方法；`String` 本身即 IntoElement。
                .text_color(gpui_kit::Hsla::white())
                .text_size(px(13.))
                .w_full()
                .child(text.clone())
                .into_any_element(),
        };

        root = root.child(
            div()
                .flex()
                .flex_col()
                .size_full()
                .when_some(hover_buttons, |this, buttons| {
                    // 操作菜单用 Absolute 浮在右上角，不占布局。
                    this.child(
                        div()
                            .absolute()
                            .right(px(4.))
                            .top(px(4.))
                            .flex()
                            .gap(px(2.))
                            .child(buttons),
                    )
                })
                .child(content_el),
        );

        root
    }
}

impl PinWindow {
    fn action_buttons(&self, cx: &mut Context<Self>) -> AnyElement {
        let mut row = div().flex().gap(px(2.));

        let actions = [
            (PanelAction::Copy, IconName::ClipboardCopy, "复制"),
            (PanelAction::Save, IconName::Download, "保存"),
            (
                PanelAction::TogglePin,
                if self.pinned {
                    IconName::PinOff
                } else {
                    IconName::Pin
                },
                if self.pinned {
                    "取消置顶"
                } else {
                    "置顶"
                },
            ),
            (PanelAction::Close, IconName::X, "关闭（双击）"),
        ];

        for (action, icon, label) in actions {
            let action_cloned = action.clone();
            row = row.child(
                Button::new(format!("pin-action-{:?}", action))
                    .ghost()
                    .icon(icon)
                    .small()
                    .tooltip(label)
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.run_action(&action_cloned, window, cx);
                    })),
            );
        }

        row.into_any_element()
    }
}
