//! 扩展管理窗口的视图：左侧列表、右侧详情、底部扩展级操作。
//!
//! 渲染期**不扫盘**：`inventory()` 只是内存遍历，磁盘扫描发生在窗口打开、
//! 点「刷新」、以及任何一次操作之后。状态变化（禁用/卸载/新建）通过
//! `AppShell::items_revision` 传进来，这时也只重取一次内存快照。

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{ActiveTheme as _, Icon, IconName, Sizable};
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, FontWeight, Hsla, InteractiveElement as _,
    IntoElement, ParentElement as _, Render, StatefulInteractiveElement as _, Styled as _, Window,
    div, px,
};
use plux_js_manager::RuntimeState;
use plux_rs::Bundle;

use super::super::actions::{ActionSubject, extension_actions};
use super::super::host::ExtensionEntry;
use super::super::manifest::{active_extensions_dir, extension_data_dir};
use super::super::report_to_console;
use super::super::state::ExtensionsState;
use crate::app::shell::AppShell;
use crate::ui::window::{WindowPin, WindowTitleBar};

pub(crate) struct ExtensionManagerView {
    entries: Vec<ExtensionEntry>,
    selected: Option<Bundle>,
    status: String,
    /// 标题栏右上角的置顶图钉（公共控件，读窗口真实层级）。
    pin: Entity<WindowPin>,
    /// 上次看到的 items 版本号：外部操作（Launcher 动作、新建扩展）改过状态后
    /// 靠它决定要不要重取快照。
    seen_revision: u64,
}

impl ExtensionManagerView {
    pub(crate) fn new(cx: &mut App) -> Self {
        let mut view = Self {
            entries: Vec::new(),
            selected: None,
            status: String::new(),
            pin: cx.new(|_| WindowPin::new()),
            seen_revision: 0,
        };
        view.refresh(cx);
        view
    }

    /// 扫盘 + 重取快照。只在开窗、点刷新、操作之后调用。
    fn refresh(&mut self, cx: &mut App) {
        let (entries, report) = {
            let mut host = cx.global::<ExtensionsState>().host.borrow_mut();
            let report = host.refresh();
            (host.inventory(), report)
        };
        report_to_console(cx, &report);
        self.entries = entries;
        self.seen_revision = cx.global::<AppShell>().items_revision.get();
        self.sync_selection();
        let disabled = self.entries.iter().filter(|entry| !entry.enabled).count();
        self.status = format!("共 {} 个扩展，{disabled} 个已禁用", self.entries.len());
    }

    /// 只重取内存快照（外部操作刚改过状态时用）。
    fn sync_revision(&mut self, cx: &App) {
        let revision = cx.global::<AppShell>().items_revision.get();
        if revision == self.seen_revision {
            return;
        }
        self.seen_revision = revision;
        self.entries = cx.global::<ExtensionsState>().host.borrow().inventory();
        self.sync_selection();
    }

    fn sync_selection(&mut self) {
        let alive = self
            .selected
            .as_ref()
            .is_some_and(|bundle| self.entries.iter().any(|entry| &entry.bundle == bundle));
        if !alive {
            self.selected = self.entries.first().map(|entry| entry.bundle.clone());
        }
    }

    fn selected_entry(&self) -> Option<&ExtensionEntry> {
        let selected = self.selected.as_ref()?;
        self.entries.iter().find(|entry| &entry.bundle == selected)
    }

    fn select(&mut self, bundle: Bundle, cx: &mut Context<Self>) {
        self.selected = Some(bundle);
        cx.notify();
    }

    // ── 渲染 ────────────────────────────────────────────────────────────

    fn header(&self, cx: &mut Context<Self>) -> AnyElement {
        WindowTitleBar::new("扩展管理")
            .subtitle(self.status.clone())
            // 右侧控件按声明顺序水平排列（公共壳自己包行容器）。
            .trailing(
                Button::new("ext-manager-new")
                    .icon(IconName::Plus)
                    .small()
                    .ghost()
                    .tooltip("新建扩展")
                    .on_click(cx.listener(|_, _, _, cx| {
                        crate::modules::extensions::create::window::open_create(cx)
                    })),
            )
            .trailing(
                Button::new("ext-manager-refresh")
                    .icon(IconName::RefreshCw)
                    .small()
                    .ghost()
                    .tooltip("刷新")
                    .on_click(cx.listener(|this, _, _, cx| this.refresh(cx))),
            )
            .pin(self.pin.clone())
            .into_any_element()
    }

    fn list(&self, cx: &mut Context<Self>) -> AnyElement {
        let colors = cx.theme().colors;
        let mut column = div()
            .flex()
            .flex_col()
            .w(px(300.))
            .flex_shrink_0()
            .border_r_1()
            .border_color(colors.border)
            .py(px(6.));

        if self.entries.is_empty() {
            return column
                .child(
                    div()
                        .px(px(12.))
                        .py(px(8.))
                        .text_size(px(12.))
                        .text_color(colors.muted_foreground)
                        .child(format!(
                            "{} 下还没有扩展",
                            active_extensions_dir().display()
                        )),
                )
                .into_any_element();
        }

        for (index, entry) in self.entries.iter().enumerate() {
            let selected = self.selected.as_ref() == Some(&entry.bundle);
            let bundle = entry.bundle.clone();
            let row_bg = if selected {
                colors.accent
            } else {
                colors.background
            };
            column = column.child(
                div()
                    .id(("ext-row", index))
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .px(px(12.))
                    .py(px(7.))
                    .bg(row_bg)
                    .hover(|style| style.bg(colors.muted.opacity(0.5)).cursor_pointer())
                    .on_click(cx.listener(move |this, _, _, cx| this.select(bundle.clone(), cx)))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .child(match &entry.icon {
                                Some(icon) => {
                                    Icon::new(crate::app::assets::AssetIcon(icon.clone().into()))
                                        .size_4()
                                        .into_any_element()
                                }
                                None => Icon::new(gpui_kit::assets::IconName::Puzzle)
                                    .size_4()
                                    .into_any_element(),
                            })
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_size(px(12.))
                                    .text_color(colors.foreground)
                                    .child(entry.name.clone()),
                            )
                            .child(
                                div()
                                    .text_size(px(10.))
                                    .text_color(colors.muted_foreground)
                                    .child(format!("v{}", entry.bundle.version)),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(4.))
                            .child(badge(&entry.bundle.id, colors.muted_foreground))
                            .children(self.badges(
                                entry,
                                colors.muted_foreground,
                                colors.danger,
                                colors.primary,
                            )),
                    ),
            );
        }

        column.into_any_element()
    }

    fn badges(
        &self,
        entry: &ExtensionEntry,
        muted: Hsla,
        destructive: Hsla,
        primary: Hsla,
    ) -> Vec<AnyElement> {
        let mut badges = Vec::new();
        if !entry.enabled {
            badges.push(badge("已禁用", muted));
        }
        if entry.manifest_error.is_some() {
            badges.push(badge("异常", destructive));
        }
        if entry.shadowed {
            badges.push(badge("被遮蔽", muted));
        }
        match entry.runtime {
            Some(RuntimeState::Ready) => badges.push(badge("运行中", primary)),
            Some(RuntimeState::Dead) => badges.push(badge("已退出", destructive)),
            _ => {}
        }
        badges
    }

    fn detail(&self, cx: &Context<Self>) -> AnyElement {
        let colors = cx.theme().colors;
        let Some(entry) = self.selected_entry() else {
            return div()
                .flex_1()
                .p(px(20.))
                .text_size(px(12.))
                .text_color(colors.muted_foreground)
                .child("选一个扩展查看详情")
                .into_any_element();
        };

        let mut column = div()
            .flex()
            .flex_col()
            .gap(px(10.))
            .flex_1()
            .min_w_0()
            .p(px(20.));

        column = column.child(
            div()
                .flex()
                .items_baseline()
                .gap(px(8.))
                .child(
                    div()
                        .text_size(px(16.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(colors.foreground)
                        .child(entry.name.clone()),
                )
                .child(
                    div()
                        .text_size(px(11.))
                        .text_color(colors.muted_foreground)
                        .child(format!("v{}", entry.bundle.version)),
                ),
        );

        if let Some(description) = &entry.description {
            column = column.child(field_text(description, colors.muted_foreground));
        }

        column = column
            .child(field_row(
                "状态",
                if entry.enabled {
                    if entry.registered {
                        "启用 · 已注册"
                    } else {
                        "启用 · 未注册"
                    }
                } else {
                    "已禁用（不注册、不加载）"
                },
                colors.muted_foreground,
                colors.foreground,
            ))
            .child(field_row(
                "命令",
                &format!(
                    "{} 条（其中 UI 命令 {} 条）",
                    entry.command_count, entry.ui_command_count
                ),
                colors.muted_foreground,
                colors.foreground,
            ))
            .child(field_row(
                "权限",
                &if entry.permissions.is_empty() {
                    "无（fail-closed：危险 API 调用时抛错）".to_string()
                } else {
                    entry.permissions.join("、")
                },
                colors.muted_foreground,
                colors.foreground,
            ))
            .child(field_row(
                "扩展目录",
                &entry.root.display().to_string(),
                colors.muted_foreground,
                colors.foreground,
            ))
            .child(field_row(
                "数据目录",
                &extension_data_dir(&entry.bundle.id).display().to_string(),
                colors.muted_foreground,
                colors.foreground,
            ));

        if !entry.dependents.is_empty() {
            let names: Vec<String> = entry
                .dependents
                .iter()
                .map(|bundle| format!("{} v{}", bundle.id, bundle.version))
                .collect();
            column = column.child(field_row(
                "被依赖",
                &names.join("、"),
                colors.muted_foreground,
                colors.foreground,
            ));
        }

        if let Some(error) = &entry.manifest_error {
            column = column.child(field_block("清单错误", error, colors.danger));
        }
        if let Some(error) = &entry.last_error {
            column = column.child(field_block("最近错误", error, colors.danger));
        }

        column.into_any_element()
    }

    fn footer(&self, cx: &mut Context<Self>) -> AnyElement {
        let colors = cx.theme().colors;
        let mut bar = div()
            .flex()
            .items_center()
            .gap(px(6.))
            .px(px(12.))
            .py(px(8.))
            .border_t_1()
            .border_color(colors.border);

        let Some(entry) = self.selected_entry() else {
            return bar.into_any_element();
        };
        let subject = ActionSubject::from_entry(entry);

        for (index, action) in extension_actions(&subject).into_iter().enumerate() {
            let dialog = action.dialog.clone();
            let run = action.run.clone();
            bar = bar.child(
                Button::new(("ext-action", index))
                    .label(action.title.clone())
                    .ghost()
                    .compact()
                    .on_click(cx.listener(move |this, _, window, cx| {
                        match &dialog {
                            Some(spec) => {
                                crate::app::launcher::actions::open_action_dialog(window, cx, spec)
                            }
                            None => run(cx),
                        }
                        this.refresh(cx);
                    })),
            );
        }

        bar.child(div().flex_1())
            .child(
                div()
                    .text_size(px(10.))
                    .text_color(colors.muted_foreground)
                    .child("禁用/卸载会立即释放运行时（加载中的扩展最多等 0.5s）"),
            )
            .into_any_element()
    }
}

impl Render for ExtensionManagerView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_revision(cx);
        let colors = cx.theme().colors;
        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(colors.background)
            .child(self.header(cx))
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .child(self.list(cx))
                    .child(self.detail(cx)),
            )
            .child(self.footer(cx))
    }
}

fn badge(text: &str, color: Hsla) -> AnyElement {
    div()
        .px(px(5.))
        .py(px(1.))
        .rounded(px(4.))
        .bg(color.opacity(0.14))
        .text_size(px(9.5))
        .text_color(color)
        .child(text.to_string())
        .into_any_element()
}

fn field_text(text: &str, color: Hsla) -> AnyElement {
    div()
        .text_size(px(12.))
        .text_color(color)
        .child(text.to_string())
        .into_any_element()
}

fn field_row(label: &str, value: &str, label_color: Hsla, value_color: Hsla) -> AnyElement {
    div()
        .flex()
        .items_start()
        .gap(px(10.))
        .child(
            div()
                .w(px(64.))
                .flex_shrink_0()
                .text_size(px(11.))
                .text_color(label_color)
                .child(label.to_string()),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_size(px(12.))
                .text_color(value_color)
                .child(value.to_string()),
        )
        .into_any_element()
}

fn field_block(label: &str, value: &str, color: Hsla) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .gap(px(3.))
        .child(
            div()
                .text_size(px(11.))
                .text_color(color.opacity(0.75))
                .child(label.to_string()),
        )
        .child(
            div()
                .p(px(8.))
                .rounded(px(5.))
                .bg(color.opacity(0.08))
                .text_size(px(11.))
                .text_color(color)
                .child(value.to_string()),
        )
        .into_any_element()
}
