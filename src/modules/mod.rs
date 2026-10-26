//! Feature modules — each a self-contained unit that contributes items to the
//! launcher and owns its own windows and state. The app shell wires them up at
//! boot via [`default_modules`].

pub(crate) mod apps;
pub(crate) mod color_picker;
pub(crate) mod extensions;
pub(crate) mod json_editor;
pub(crate) mod pin;
pub(crate) mod scripts;

/// 默认模块表：按出现顺序 collect，boot 时注入 `AppShell`。
pub(crate) fn default_modules(
    cx: &mut gpui_kit::App,
) -> Vec<Box<dyn crate::app::launcher::Module>> {
    vec![
        Box::new(apps::AppsModule::new(cx)),
        Box::new(scripts::ScriptsModule),
        Box::new(json_editor::JsonEditorModule),
        Box::new(pin::PinModule),
        Box::new(color_picker::ColorPickerModule),
        Box::new(extensions::ExtensionsModule),
    ]
}
