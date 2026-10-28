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
use crate::ui::window_pin::{move_window_origin, set_window_level};

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
    /// 否则图片会被窗口裁掉或者四周露底。缩放以**光标为锚点**（参考
    /// macshot）：光标下的内容缩放前后保持在同一屏幕位置，光标即放大镜
    /// 中心，不用边滚边挪窗口。
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
        self.apply_zoom(next, event.position, window, cx);
        cx.stop_propagation();
    }

    /// 应用新的缩放倍率：窗口同步 resize（否则图片被裁/露底），并按光标
    /// 锚点调整窗口位置——resize 锚定在 macOS 左下角（视觉上顶边不动），
    /// 光标下的内容会漂移 fx·Δsize，把窗口反向平移回去。cursor 是窗口内
    /// 坐标（越界时公式照样外推成立）。滚轮缩放与「点徽标回 100%」共用。
    fn apply_zoom(
        &mut self,
        next: f32,
        cursor: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let old = window.bounds();
        self.zoom = next;
        let (w, h) = self.scaled_size();
        window.resize(size(px(w), px(h)));
        if old.size.width > px(0.) && old.size.height > px(0.) {
            let fx = cursor.x / old.size.width;
            let fy = cursor.y / old.size.height;
            let dx = fx * (old.size.width - px(w));
            let dy = fy * (old.size.height - px(h));
            move_window_origin(window, dx.to_f64(), dy.to_f64());
        }
        cx.notify();
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
            PinContent::Image { bytes, format, .. } => {
                // 按原始格式塞回剪切板（读取时不转码，字节就是来源格式）；
                // GPUI 的 ClipboardItem 没有 From<Vec<u8>>。
                let image = Image::from_bytes(*format, bytes.clone());
                cx.write_to_clipboard(image.into());
            }
            PinContent::Text(text) => cx.write_to_clipboard(text.clone().into()),
        }
    }

    /// 保存到用户选的路径：弹原生保存面板（`prompt_for_new_path`），
    /// 默认目录为桌面、默认文件名为 `pin-<时间戳>.<扩展名按来源格式>`。
    fn save_to_disk(&mut self, cx: &mut Context<Self>) {
        let (payload, ext) = match &self.content {
            PinContent::Image { bytes, format, .. } => (
                bytes.clone(),
                match format {
                    ImageFormat::Png => "png",
                    ImageFormat::Tiff => "tiff",
                    ImageFormat::Jpeg => "jpg",
                    ImageFormat::Gif => "gif",
                    ImageFormat::Bmp => "bmp",
                    ImageFormat::Ico => "ico",
                    ImageFormat::Pnm => "pnm",
                    ImageFormat::Webp => "webp",
                    ImageFormat::Svg => "svg",
                },
            ),
            PinContent::Text(text) => (text.clone().into_bytes(), "txt"),
        };
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let suggested = format!("pin-{ts}.{ext}");
        let receiver = cx.prompt_for_new_path(&clipboard::default_save_dir(), Some(&suggested));

        // 原生面板是异步的：离开面板后由 oneshot 带回结果（取消 = None）。
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(path))) = receiver.await else {
                return;
            };
            let result = std::fs::write(&path, payload);
            let _ = this.update(cx, |_, cx| {
                let console = cx
                    .global::<crate::app::shell::AppShell>()
                    .console_channel
                    .clone();
                match result {
                    Ok(()) => console.info(format!("已保存到 {}", path.display())),
                    Err(e) => console.error(format!("保存失败 {e}")),
                }
                cx.notify();
            });
        })
        .detach();
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

        // 窗口级按键（窗口没有 keymap binding，keydown listener 每帧都能
        // 收到）：Esc 关闭便签；Cmd+Q 就地拦截为关闭便签（参考 macshot）——
        // 贴图连按 Cmd+Q 会把整个应用退掉，这不是用户预期。Capture 阶段
        // 最先拿到事件。
        //
        // `on_key_event` 断言只能在 paint 期调用，而 `Render::render` 在
        // 布局期就执行了——挂在一个零尺寸 `canvas` 的 paint 回调里（每帧
        // 重建注册一次，与 "next frame" 语义一致）。
        root = root.child(gpui_kit::canvas(
            |_, _, _| (),
            |_, _, window: &mut Window, _: &mut App| {
                window.on_key_event(
                    |event: &KeyDownEvent,
                     phase: DispatchPhase,
                     window: &mut Window,
                     _: &mut App| {
                        let hit = event.keystroke.key == "escape"
                            || (event.keystroke.key == "q" && event.keystroke.modifiers.platform);
                        if hit && phase == DispatchPhase::Capture {
                            window.remove_window();
                        }
                    },
                );
            },
        ));

        // 右上角操作菜单（悬停时浮现）。
        let hover_buttons = if self.hovered {
            Some(self.action_buttons(cx))
        } else {
            None
        };

        // 内容区域：按类型渲染。
        let content_el = match &self.content {
            PinContent::Image { bytes, format, .. } => {
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
                let image = Arc::new(Image::from_bytes(*format, bytes.clone()));
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
                    // 需要 `.id()`：`on_click` 只挂在 `Stateful<Div>` 上。
                    .id("pin-zoom-badge")
                    .h(px(24.))
                    .px(px(9.))
                    .rounded_full()
                    .bg(hsla(0., 0., 0., 0.9))
                    .flex()
                    .items_center()
                    .text_color(Hsla::white())
                    .text_size(px(11.))
                    // 点击徽标一键回 100%（macshot 同款；已 100% 则无操作）。
                    // 拦下 mouse_down 防止触发整窗拖动 / 双击关闭。
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(|this, event: &ClickEvent, window, cx| {
                        if (this.zoom - 1.).abs() < 1e-4 {
                            return;
                        }
                        this.apply_zoom(1., event.position(), window, cx);
                    }))
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
