mod console_channel;
mod host_module;
mod host_modules;
mod lsp;
mod lsp_providers;
mod panels;
mod project_gen;
mod runner;
mod script;
mod script_store;
mod ts_transform;
mod workspace;

use std::sync::Arc;

use gpui_kit::*;

use crate::host_module::HostRegistry;

fn main() {
    let scripts_dir = script_store::scripts_dir();
    if let Err(e) = script_store::ensure_dir(&scripts_dir) {
        eprintln!("ensure scripts dir: {e}");
    }

    // Register built-in host modules before generating project files so
    // their .d.ts declarations are available for IDE/LSP.
    let mut registry = HostRegistry::new();
    host_modules::register_all(&mut registry);
    if let Err(e) = project_gen::generate_project_files(&scripts_dir, &registry) {
        eprintln!("generate project files: {e}");
    }
    let registry = Arc::new(registry);

    // Spawn the language server before the UI so the editor gets diagnostics,
    // completions and hover from the start. Degrades to highlighting-only
    // when typescript-language-server is not installed.
    let lsp_client = lsp::LspClient::spawn(&scripts_dir).map(Arc::new);

    gpui_kit::application().run(move |cx| {
        gpui_kit::init(cx);

        gpui_kit::open_window(WindowOptions::default(), cx, |window, cx| {
            cx.new(|cx| workspace::Workspace::new(window, cx, registry.clone(), lsp_client))
        })
        .expect("Failed to open window");
    });
}
