//! 功能模块——每个都是自包含单元：向 launcher 贡献 items，并拥有自己的窗口
//! 与状态。app shell 在启动时经 [`default_modules`] 把它们装配起来。

pub(crate) mod apps;
pub(crate) mod color_picker;
pub(crate) mod console;
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
        Box::new(console::ConsoleModule),
    ]
}
