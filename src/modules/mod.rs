//! Feature modules — each a self-contained unit that contributes items to the
//! launcher and owns its own windows and state. The app shell wires them up at
//! boot via [`default_modules`].

pub(crate) mod extensions;
pub(crate) mod json_editor;
pub(crate) mod pin;
pub(crate) mod scripts;

/// 默认模块表：按出现顺序 collect，boot 时注入 `AppShell`。
pub(crate) fn default_modules() -> Vec<Box<dyn crate::app::launcher::Module>> {
    vec![
        Box::new(scripts::ScriptsModule),
        Box::new(json_editor::JsonEditorModule),
        Box::new(pin::PinModule),
        Box::new(extensions::ExtensionsModule),
    ]
}
