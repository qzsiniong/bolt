//! 「新建扩展」表单：id / 名称 / 版本 / 模板 / 权限，实时校验并预览目录名。
//!
//! 字段合法性由 [`scaffold::validate`] 一处定义（含"把目录名再解析回来"的权威
//! 校验），这里只负责把错误摆到对应输入框下面；创建时同一份校验会再跑一遍，
//! 避免 UI 与落盘规则漂移。

use std::path::PathBuf;

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::Disableable as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, Hsla, IntoElement, ParentElement as _,
    Render, Styled as _, Subscription, Window, div, px,
};

use super::super::manifest::{active_extensions_dir, uses_bundled_extensions};
use super::super::report_to_console;
use super::super::scaffold::{self, ExtensionSpec, Field, SpecError, Template};
use super::super::state::ExtensionsState;
use crate::app::windows;
use crate::ui::mac_ffi;
use crate::ui::window::{WindowPin, WindowTitleBar};

pub(crate) struct CreateExtensionView {
    /// 标题栏右上角的置顶图钉（公共控件，读窗口真实层级）。
    pin: Entity<WindowPin>,
    id: Entity<InputState>,
    name: Entity<InputState>,
    version: Entity<InputState>,
    template: Template,
    permissions: Vec<String>,
    clipboard: bool,
    /// 创建失败的原因，或成功后的目录。
    outcome: Option<Result<PathBuf, String>>,
    _subscriptions: Vec<Subscription>,
}

impl CreateExtensionView {
    pub(crate) fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let id = cx.new(|cx| {
            InputState::new(window, cx).placeholder("小写字母、数字、连字符，例如 my-tool")
        });
        let name = cx.new(|cx| InputState::new(window, cx).placeholder("展示名，留空用 id"));
        let version = cx.new(|cx| InputState::new(window, cx).placeholder("1.0.0"));
        version.update(cx, |state, cx| {
            state.set_value("1.0.0", window, cx);
        });

        let mut view = Self {
            pin: cx.new(|_| WindowPin::new()),
            id,
            name,
            version,
            template: Template::Script,
            permissions: Vec::new(),
            clipboard: false,
            outcome: None,
            _subscriptions: Vec::new(),
        };

        // 输入变化只影响校验与预览，`cx.notify()` 就够（不需要重扫磁盘）。
        view._subscriptions = vec![
            cx.subscribe(&view.id, |this, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    this.outcome = None;
                    cx.notify();
                }
            }),
            cx.subscribe(&view.name, |this, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    this.outcome = None;
                    cx.notify();
                }
            }),
            cx.subscribe(&view.version, |this, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    this.outcome = None;
                    cx.notify();
                }
            }),
        ];

        view
    }

    fn spec(&self, cx: &App) -> ExtensionSpec {
        ExtensionSpec {
            id: self.id.read(cx).value().to_string(),
            name: self.name.read(cx).value().to_string(),
            version: self.version.read(cx).value().to_string(),
            template: self.template,
            permissions: self.permissions.clone(),
            clipboard: self.clipboard,
        }
    }

    /// 当前字段拼出来的目录，是否就是刚创建成功的那个。
    ///
    /// 建完之后目录当然"已存在"，但那不是冲突——不区分的话，创建成功的下一秒
    /// 表单里就挂出一条红字「目录已存在」，看着像失败了。
    fn just_created(&self, cx: &App) -> bool {
        let target = scaffold::target_dir(&self.spec(cx));
        matches!(&self.outcome, Some(Ok(root)) if *root == target)
    }

    /// 「创建」是否可点：字段合法、目标目录没被占、也不是刚建完的那个。
    fn can_create(&self, cx: &App) -> bool {
        let spec = self.spec(cx);
        scaffold::validate(&spec).is_ok()
            && !self.just_created(cx)
            && scaffold::conflict(&spec).is_none()
    }

    fn create(&mut self, cx: &mut Context<Self>) {
        let spec = self.spec(cx);
        // 刚建完、字段没动过：再点一次只会得到"目录已存在"。直接吞掉，
        // 双击不会把成功提示换成一条报错。
        if matches!(&self.outcome, Some(Ok(root)) if *root == scaffold::target_dir(&spec)) {
            return;
        }
        let registry = cx.global::<ExtensionsState>().registry.clone();

        let result =
            scaffold::scaffold(&active_extensions_dir(), &spec, &registry).map(|outcome| {
                let report = {
                    let mut host = cx.global::<ExtensionsState>().host.borrow_mut();
                    host.refresh()
                };
                report_to_console(cx, &report);
                // 让已打开的 Launcher 立刻收到新命令，不用等下次唤出。
                windows::items_changed(cx);
                outcome.root
            });

        self.outcome = Some(result);
        cx.notify();
    }

    fn toggle_permission(&mut self, name: &str) {
        if let Some(index) = self.permissions.iter().position(|item| item == name) {
            self.permissions.remove(index);
        } else {
            self.permissions.push(name.to_string());
        }
    }

    // ── 渲染 ────────────────────────────────────────────────────────────

    fn header(&self, _cx: &Context<Self>) -> AnyElement {
        WindowTitleBar::new("新建扩展")
            .pin(self.pin.clone())
            .into_any_element()
    }

    fn body(&self, cx: &mut Context<Self>) -> AnyElement {
        let colors = cx.theme().colors;
        let spec = self.spec(cx);
        let errors = scaffold::validate(&spec).err().unwrap_or_default();
        let conflict = if self.just_created(cx) {
            None
        } else {
            scaffold::conflict(&spec)
        };
        let warning = scaffold::shadow_warning(&spec);

        let mut column = div()
            .flex()
            .flex_col()
            .gap(px(12.))
            .flex_1()
            .min_h_0()
            .p(px(20.));

        column = column
            .child(self.field_row(
                "id",
                &self.id,
                field_error(&errors, Field::Id),
                colors.foreground,
                colors.danger,
                colors.muted_foreground,
            ))
            .child(self.field_row(
                "名称",
                &self.name,
                field_error(&errors, Field::Name),
                colors.foreground,
                colors.danger,
                colors.muted_foreground,
            ))
            .child(self.field_row(
                "版本",
                &self.version,
                field_error(&errors, Field::Version),
                colors.foreground,
                colors.danger,
                colors.muted_foreground,
            ));

        // 模板
        let templates = div().flex().flex_col().gap(px(4.)).child(
            div()
                .text_size(px(11.))
                .text_color(colors.muted_foreground)
                .child("模板"),
        );
        let mut row = div().flex().gap(px(6.));
        for (index, template) in Template::ALL.into_iter().enumerate() {
            let selected = self.template == template;
            let mut button = Button::new(("template", index))
                .label(template.label())
                .compact()
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.template = template;
                    if !template.has_ui() {
                        this.clipboard = false;
                    }
                    if !template.has_script() {
                        this.permissions.clear();
                    }
                    this.outcome = None;
                    cx.notify();
                }));
            button = if selected {
                button.primary()
            } else {
                button.ghost()
            };
            row = row.child(button);
        }
        column = column.child(templates.child(row));

        // 脚本权限
        if self.template.has_script() {
            let permissions = div().flex().flex_col().gap(px(4.)).child(
                div()
                    .text_size(px(11.))
                    .text_color(colors.muted_foreground)
                    .child("脚本权限（缺省一个都不给：没声明的危险 API 调用时抛错）"),
            );
            let mut row = div().flex().flex_wrap().gap(px(6.));
            for (index, name) in plux_js_manager::PERMISSIONS.iter().enumerate() {
                let selected = self.permissions.iter().any(|item| item == name);
                let name = *name;
                let mut button = Button::new(("permission", index))
                    .label(name)
                    .compact()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.toggle_permission(name);
                        this.outcome = None;
                        cx.notify();
                    }));
                button = if selected {
                    button.primary()
                } else {
                    button.ghost()
                };
                row = row.child(button);
            }
            column = column.child(permissions.child(row));
        }

        // UI 剪贴板能力
        if self.template.has_ui() {
            let selected = self.clipboard;
            let mut button = Button::new("clipboard")
                .label(if selected {
                    "剪贴板：已授权"
                } else {
                    "剪贴板：未授权"
                })
                .compact()
                .on_click(cx.listener(|this, _, _, cx| {
                    this.clipboard = !this.clipboard;
                    this.outcome = None;
                    cx.notify();
                }));
            button = if selected {
                button.primary()
            } else {
                button.ghost()
            };
            column = column.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(
                        div()
                            .text_size(px(11.))
                            .text_color(colors.muted_foreground)
                            .child("UI 能力"),
                    )
                    .child(button),
            );
        }

        // 预览
        column = column.child(
            div()
                .flex()
                .flex_col()
                .gap(px(3.))
                .child(
                    div()
                        .text_size(px(11.))
                        .text_color(colors.muted_foreground)
                        .child("目标目录"),
                )
                .child(
                    div()
                        .text_size(px(11.))
                        .text_color(colors.foreground)
                        // 与 scaffold::target_dir 同源：debug 下就是仓库 extensions/。
                        .child(scaffold::target_dir(&spec).display().to_string()),
                ),
        );

        for error in &errors {
            column = column.child(line(error.message.clone(), colors.danger));
        }
        if let Some(conflict) = conflict {
            column = column.child(line(conflict, colors.danger));
        }
        if let Some(warning) = warning {
            column = column.child(line(warning, colors.muted_foreground));
        }

        if uses_bundled_extensions() {
            column = column.child(line(
                format!(
                    "开发构建：扩展会写进仓库 {}，会出现在 git status 里",
                    active_extensions_dir().display()
                ),
                colors.muted_foreground,
            ));
        }

        column.into_any_element()
    }

    fn field_row(
        &self,
        label: &str,
        input: &Entity<InputState>,
        error: Option<&str>,
        _text: Hsla,
        danger: Hsla,
        muted: Hsla,
    ) -> AnyElement {
        div()
            .flex()
            .flex_col()
            .gap(px(3.))
            .child(
                div()
                    .text_size(px(11.))
                    .text_color(muted)
                    .child(label.to_string()),
            )
            .child(Input::new(input))
            .child(
                div()
                    .text_size(px(10.))
                    .text_color(danger)
                    .child(error.unwrap_or_default().to_string()),
            )
            .into_any_element()
    }

    fn footer(&self, cx: &mut Context<Self>) -> AnyElement {
        let colors = cx.theme().colors;
        let mut bar = div()
            .flex()
            .items_center()
            .gap(px(8.))
            .px(px(16.))
            .py(px(10.))
            .border_t_1()
            .border_color(colors.border);

        match &self.outcome {
            Some(Ok(root)) => {
                let root = root.clone();
                bar = bar
                    .child(
                        div()
                            .flex_1()
                            .text_size(px(11.))
                            .text_color(colors.muted_foreground)
                            .child(format!(
                                "已创建：{}（改 id 或版本号可再建一个）",
                                root.display()
                            )),
                    )
                    .child(
                        Button::new("reveal-created")
                            .label("在 Finder 中显示")
                            .ghost()
                            .compact()
                            // 同 ops::reveal：拉起 Finder 放到工作线程，别在点击
                            // 处理器里同步激活别的 App（会重入 GPUI 的 App 借用）。
                            .on_click(cx.listener(move |_, _, _, _| {
                                let target = root.clone();
                                mac_ffi::reveal_in_background(root.clone(), move |result| {
                                    if let Err(error) = result {
                                        mac_ffi::notify(
                                            "Bolt",
                                            &format!("无法显示 {}：{error}", target.display()),
                                        );
                                    }
                                });
                            })),
                    );
            }
            Some(Err(error)) => {
                bar = bar.child(
                    div()
                        .flex_1()
                        .text_size(px(11.))
                        .text_color(colors.danger)
                        .child(error.clone()),
                );
            }
            None => {
                bar = bar.child(div().flex_1());
            }
        }

        bar.child(
            Button::new("create-extension")
                .label("创建")
                .primary()
                .compact()
                // 字段不合法 / 目录被占 / 刚建完同一个：按钮置灰而不是让人点了
                // 再吃一条红字报错。改 id 或版本号会清掉 outcome，自动恢复。
                .disabled(!self.can_create(cx))
                .on_click(cx.listener(|this, _, _, cx| this.create(cx))),
        )
        .into_any_element()
    }
}

impl Render for CreateExtensionView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors;
        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(colors.background)
            .child(self.header(cx))
            .child(self.body(cx))
            .child(self.footer(cx))
    }
}

fn field_error(errors: &[SpecError], field: Field) -> Option<&str> {
    errors
        .iter()
        .find(|error| error.field == field)
        .map(|error| error.message.as_str())
}

fn line(text: String, color: Hsla) -> AnyElement {
    div()
        .text_size(px(11.))
        .text_color(color)
        .child(text)
        .into_any_element()
}
