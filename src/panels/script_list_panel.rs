use std::path::PathBuf;

use gpui_kit::base::dock::PanelEvent;
use gpui_kit::component::ActiveTheme;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::dock::{BasePanel, Panel};
use gpui_kit::*;

use crate::script::{Language, Script};
use crate::script_store;

/// Emitted when the user picks a script from the list.
#[derive(Clone, Debug)]
pub struct ScriptSelected(pub Script);

pub struct ScriptListPanel {
    focus_handle: FocusHandle,
    scripts_dir: PathBuf,
    scripts: Vec<Script>,
    selected_file: Option<String>,
    confirming_delete: Option<String>,
}

impl ScriptListPanel {
    pub fn new(_window: &mut Window, cx: &mut Context<Self>) -> Self {
        let scripts_dir = script_store::scripts_dir();
        let scripts = script_store::list_scripts(&scripts_dir);
        Self {
            focus_handle: cx.focus_handle(),
            scripts_dir,
            scripts,
            selected_file: None,
            confirming_delete: None,
        }
    }

    pub fn scripts_dir(&self) -> &PathBuf {
        &self.scripts_dir
    }

    pub fn selected(&self) -> Option<&Script> {
        self.scripts
            .iter()
            .find(|s| Some(&s.file_name) == self.selected_file.as_ref())
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        self.scripts = script_store::list_scripts(&self.scripts_dir);
        cx.notify();
    }

    fn select(&mut self, file_name: String, cx: &mut Context<Self>) {
        if Some(&file_name) == self.selected_file.as_ref() {
            return;
        }
        self.selected_file = Some(file_name.clone());
        if let Some(script) = self
            .scripts
            .iter()
            .find(|s| s.file_name == file_name)
            .cloned()
        {
            cx.emit(ScriptSelected(script));
        }
        cx.notify();
    }

    /// Create a new script file (toolbar + button / Cmd+N).
    pub fn create_new(&mut self, cx: &mut Context<Self>) {
        let base = "untitled";
        let mut idx = 1;
        loop {
            let candidate = format!("{base}-{idx}");
            if !self.scripts.iter().any(|s| s.name == candidate) {
                match script_store::create_script(
                    &self.scripts_dir,
                    &candidate,
                    Language::TypeScript,
                ) {
                    Ok(script) => {
                        let file_name = script.file_name.clone();
                        self.refresh(cx);
                        self.select(file_name, cx);
                        return;
                    }
                    Err(e) => {
                        eprintln!("create_script: {e}");
                        return;
                    }
                }
            }
            idx += 1;
        }
    }

    fn request_delete(&mut self, file_name: String, cx: &mut Context<Self>) {
        self.confirming_delete = Some(file_name);
        cx.notify();
    }

    /// Open the scripts directory in Finder.
    fn reveal_scripts_dir(&mut self, _cx: &mut Context<Self>) {
        if let Err(e) = script_store::reveal_in_finder(&self.scripts_dir, false) {
            eprintln!("reveal scripts dir: {e}");
        }
    }

    /// Select a script file in Finder.
    fn reveal_script(&mut self, file_name: String, _cx: &mut Context<Self>) {
        let path = self.scripts_dir.join(file_name);
        if let Err(e) = script_store::reveal_in_finder(&path, true) {
            eprintln!("reveal script: {e}");
        }
    }

    fn cancel_delete(&mut self, cx: &mut Context<Self>) {
        self.confirming_delete = None;
        cx.notify();
    }

    fn confirm_delete(&mut self, file_name: String, cx: &mut Context<Self>) {
        if let Err(e) = script_store::delete_script(&self.scripts_dir, &file_name) {
            eprintln!("delete: {e}");
        }
        self.scripts.retain(|s| s.file_name != file_name);
        if self.selected_file.as_deref() == Some(file_name.as_str()) {
            self.selected_file = None;
        }
        self.confirming_delete = None;
        cx.notify();
    }
}

impl BasePanel for ScriptListPanel {
    fn panel_name(&self) -> &'static str {
        "ScriptList"
    }
}

impl Panel for ScriptListPanel {
    fn title(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        "Scripts".into_any_element()
    }
}

impl EventEmitter<PanelEvent> for ScriptListPanel {}
impl EventEmitter<ScriptSelected> for ScriptListPanel {}

impl Focusable for ScriptListPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for ScriptListPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let border = theme.border;
        let muted_fg = theme.muted_foreground;
        let danger = theme.danger;
        let accent = theme.accent;
        let muted = theme.muted;

        let header = div()
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
                    .child("Scripts"),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(
                        Button::new("header-reveal")
                            .icon(gpui_kit::component::IconName::FolderOpen)
                            .compact()
                            .ghost()
                            .tooltip("Reveal in Finder")
                            .on_click(cx.listener(|this, _, _, cx| this.reveal_scripts_dir(cx))),
                    )
                    .child(
                        Button::new("header-new")
                            .icon(gpui_kit::component::IconName::Plus)
                            .compact()
                            .ghost()
                            .tooltip("New script")
                            .on_click(cx.listener(|this, _, _, cx| this.create_new(cx))),
                    ),
            );

        let mut list = div().flex().flex_col();
        if self.scripts.is_empty() {
            list = list.child(
                div()
                    .p_4()
                    .text_xs()
                    .text_color(muted_fg)
                    .child("No scripts yet. Click + to create one."),
            );
        } else {
            for (idx, script) in self.scripts.iter().enumerate() {
                let file_name = script.file_name.clone();
                let is_selected = Some(&file_name) == self.selected_file.as_ref();
                let is_confirming = self.confirming_delete.as_deref() == Some(file_name.as_str());
                let ext = script.language.extension();
                let name = script.name.clone();

                let row_bg = if is_selected {
                    accent.opacity(0.15)
                } else {
                    gpui::transparent_black()
                };

                let row_action: AnyElement = if is_confirming {
                    let fn_confirm = file_name.clone();
                    let fn_cancel = file_name.clone();
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .id(("del-confirm", idx))
                                .text_xs()
                                .text_color(danger)
                                .child("delete?")
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.confirm_delete(fn_confirm.clone(), cx);
                                })),
                        )
                        .child(
                            div()
                                .id(("del-cancel", idx))
                                .text_xs()
                                .text_color(muted_fg)
                                .child("cancel")
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    let _ = &fn_cancel;
                                    this.cancel_delete(cx);
                                })),
                        )
                        .into_any_element()
                } else {
                    let fn_reveal = file_name.clone();
                    div()
                        .id(("reveal", idx))
                        .child(
                            gpui_kit::component::Icon::new(
                                gpui_kit::assets::IconName::FolderOpen,
                            )
                            .size_3p5()
                            .text_color(muted_fg),
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.reveal_script(fn_reveal.clone(), cx);
                        }))
                        .into_any_element()
                };
                let fn_req = file_name.clone();
                let fn_select = file_name.clone();

                let row = div()
                    .id(("script-row", idx))
                    .px_3()
                    .py_1p5()
                    .bg(row_bg)
                    .hover(|s| s.bg(muted.opacity(0.5)).cursor_pointer())
                    .flex()
                    .items_center()
                    .justify_between()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.select(fn_select.clone(), cx);
                    }))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            // Delete icon sits next to the script name.
                            .child(
                                div()
                                    .id(("del", idx))
                                    .child(
                                        gpui_kit::component::Icon::new(
                                            gpui_kit::assets::IconName::Trash,
                                        )
                                        .size_3p5()
                                        .text_color(muted_fg),
                                    )
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.request_delete(fn_req.clone(), cx);
                                    })),
                            )
                            .child(
                                gpui_kit::component::Icon::new(
                                    gpui_kit::assets::IconName::FileCode,
                                )
                                .size_4()
                                .text_color(muted_fg),
                            )
                            .child(div().text_sm().child(name))
                            .child(div().text_xs().text_color(muted_fg).child(ext)),
                    )
                    .child(row_action);
                list = list.child(row);
            }
        }

        div()
            .size_full()
            .flex()
            .flex_col()
            .child(header)
            .child(div().flex_1().child(list))
    }
}
