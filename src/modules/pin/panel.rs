//! Pin 便签窗口视图：无边框透明 Floating，内容区域支持拖拽 / 双击关闭，
//! 以及右上角自绘的小操作菜单（悬停显示）。

use std::sync::Arc;

use gpui_kit::assets::IconName;
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::Sizable;
use gpui_kit::component::button::{Button, ButtonCustomVariant, ButtonRounded, ButtonVariants};
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
    /// 图片缩放倍率（滚轮调整，10%–800%）。文本不缩放。
    zoom: f32,
}

impl PinWindow {
    pub(crate) fn new(content: PinContent, _window: &mut Window, _cx: &mut Context<Self>) -> Self {
        Self {
            content,
            hovered: false,
            pinned: true,
            zoom: 1.,
        }
    }

    /// 当前应显示的图片尺寸（基准尺寸 × 缩放）。
    fn scaled_size(&self) -> (f32, f32) {
        let (w, h) = self.content.display_size();
        (w * self.zoom, h * self.zoom)
    }

    fn is_image(&self) -> bool {
        matches!(self.content, PinContent::Image { .. })
    }

    /// 滚轮缩放：上滚放大、下滚缩小，夹在 10%–800%。窗口同步改尺寸，
    /// 否则图片会被窗口裁掉或者四周露底。
    fn on_wheel(&mut self, event: &ScrollWheelEvent, window: &mut Window, cx: &mut Context<Self>) {
        if !self.is_image() {
            return;
        }
        let dy = match event.delta {
            ScrollDelta::Lines(point) => point.y,
            ScrollDelta::Pixels(point) => point.y / px(40.),
        };
        if dy == 0. {
            return;
        }
        let next = (self.zoom * (1. + dy * 0.1)).clamp(0.1, 8.);
        if (next - self.zoom).abs() < 1e-4 {
            return;
        }
        self.zoom = next;
        let (w, h) = self.scaled_size();
        window.resize(size(px(w), px(h)));
        cx.notify();
        cx.stop_propagation();
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
            // 滚轮缩放（仅图片内容）。
            .on_scroll_wheel(cx.listener(|this, event, window, cx| {
                this.on_wheel(event, window, cx);
            }))
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
                let (w, h) = self.scaled_size();
                let image = Arc::new(Image::from_bytes(ImageFormat::Png, bytes.clone()));
                div()
                    .child(gpui_kit::img(image).w(px(w)).h(px(h)).rounded(px(4.)))
                    .into_any_element()
            }
            PinContent::Text(text) => div()
                .size_full()
                .p(px(14.))
                // 文本便签：白底黑字（macshot 风格），深色背景下也清楚。
                .bg(Hsla::white())
                .border_1()
                .border_color(hsla(0., 0., 0., 0.12))
                .text_color(hsla(0., 0., 0., 1.))
                .text_size(px(13.))
                .child(text.clone())
                .into_any_element(),
        };

        root = root.child(
            div()
                .flex()
                .flex_col()
                .size_full()
                // 先内容、后按钮：gpui 同级后绘制的元素在上层。图片是不透明的
                // 整块，按钮若先绘制就会被图片整个盖住（表现就是「看不见」）。
                .child(content_el)
                .when_some(hover_buttons, |this, buttons| {
                    // 操作菜单用 Absolute 浮在右上角，不占布局。
                    this.child(
                        div()
                            .absolute()
                            .right(px(8.))
                            .top(px(8.))
                            .flex()
                            .gap(px(6.))
                            .child(buttons),
                    )
                }),
        );

        root
    }
}

impl PinWindow {
    fn action_buttons(&self, cx: &mut Context<Self>) -> AnyElement {
        // macshot 风格：半透明黑色圆角底 + 白色图标，悬停/按下加深。
        // 直接给 `Button` 用 `.bg()` 无效——Button 渲染时按 variant 重设背景
        // 与前景（button.rs L717-L719），只能用 `custom` variant 传颜色。
        let variant = ButtonCustomVariant::new(cx)
            .color(hsla(0., 0., 0., 0.9))
            .foreground(Hsla::white())
            .hover(hsla(0., 0., 0., 0.9))
            .active(hsla(0., 0., 0., 0.9));

        let mut row = div().flex().gap(px(6.));

        // macshot 风格的缩放比例徽标：与操作按钮同一层深色圆角底。
        if self.is_image() {
            row = row.child(
                div()
                    .h(px(24.))
                    .px(px(9.))
                    .rounded_full()
                    .bg(hsla(0., 0., 0., 0.9))
                    .flex()
                    .items_center()
                    .text_color(Hsla::white())
                    .text_size(px(11.))
                    .child(format!("{:.0}%", self.zoom * 100.)),
            );
        }

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
                    .custom(variant)
                    // .primary()
                    .icon(icon)
                    .small()
                    // 999 会被夹到「半边长」，得到正圆（ButtonRounded 没有 Full）。
                    .rounded(ButtonRounded::Size(px(999.)))
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
