//! Bolt：Raycast 风格的启动器，在 QuickJS 运行时上执行 TypeScript/JavaScript
//! 脚本。
//!
//! 模块划分（完整分组规则见 README）：
//! `app` 组装进程并掌管窗口生命周期；`ui` 放视图与前端逻辑；`model` 只剩生成
//! 的脚本工程文件（脚本类型在 `bolt-model` crate）；`runtime` 是脚本执行管线
//! （类型擦除在 `bolt-ts` crate）；`host` 是扩展框架及其内置模块。Console 通
//! 道、LSP 客户端分别是独立的 `bolt-console` / `bolt-lsp` crate。

mod app;
mod core;
mod host;
mod model;
mod modules;
mod runtime;
#[cfg(test)]
mod test_support;
mod ui;

fn main() {
    app::boot::run();
}
