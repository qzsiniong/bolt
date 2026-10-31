//! Bolt：Raycast 风格的启动器，在 QuickJS 运行时上执行 TypeScript/JavaScript
//! 脚本。
//!
//! 模块划分（完整分组规则见 README）：
//! `app` 组装进程并掌管窗口生命周期；`ui` 放视图与前端逻辑；`model` 是领域
//! 数据与磁盘布局；`runtime` 是脚本执行管线；`host` 是扩展框架及其内置模块；
//! `lsp` 是语言服务客户端。

mod app;
mod console_channel;
mod core;
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
