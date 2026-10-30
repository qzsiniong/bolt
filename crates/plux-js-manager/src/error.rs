//! Errors surfaced to the host when a plugin cannot be loaded or a command
//! cannot be executed.
//!
//! Every variant is `Clone`: the message travels from the plugin thread over a
//! channel and is reported by the host, sometimes twice (Console + launcher).

use std::time::Duration;

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum JsError {
    /// The plugin was registered but never loaded (no runtime yet).
    #[error("插件 {0} 尚未加载")]
    NotLoaded(String),

    /// The plugin bundle is unknown to the store.
    #[error("插件未注册：{0}")]
    UnknownPlugin(String),

    /// The command id is not declared in `extension.json`.
    #[error("命令不存在：{0}")]
    UnknownCommand(String),

    /// No JS function matched the command, in any of the three fallbacks.
    #[error(
        "插件没有实现命令 `{0}`（具名导出 / default 对象 / globalThis.plux.commands 均未找到）"
    )]
    MissingHandler(String),

    /// The command exceeded its deadline and was interrupted.
    #[error("命令执行超时（{} ms）", .0.as_millis())]
    Timeout(Duration),

    /// The user aborted the run.
    #[error("命令已取消")]
    Cancelled,

    /// JS threw, or the module failed to evaluate.
    #[error("{0}")]
    Script(String),

    /// Plugin discovery / validation failed.
    #[error("{0}")]
    Invalid(String),

    /// The plugin thread is gone or never started.
    #[error("插件运行时不可用：{0}")]
    Runtime(String),
}

impl JsError {
    /// Whether the error is a normal control outcome (abort / timeout) rather
    /// than a plugin bug. Hosts use it to pick the log level.
    pub fn is_control_flow(&self) -> bool {
        matches!(self, Self::Timeout(_) | Self::Cancelled)
    }
}
