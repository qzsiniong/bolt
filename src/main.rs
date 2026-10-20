//! Scripter: a Raycast-style launcher that runs TypeScript/JavaScript scripts
//! on a QuickJS runtime.
//!
//! Module layout (see README for the full grouping rules):
//! `app` assembles the process and owns window lifecycle, `ui` holds views
//! and front-end logic, `model` the domain data and on-disk layout, `runtime`
//! the script execution pipeline, `host` the extension framework and its
//! built-in modules, and `lsp` the language-server client.

mod app;
mod console_channel;
mod host;
mod lsp;
mod model;
mod modules;
mod runtime;
#[cfg(test)]
mod test_support;
mod ui;

fn main() {
    app::boot::run();
}
